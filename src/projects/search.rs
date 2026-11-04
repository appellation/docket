//! Searching a browsed tree (`files::Tree`): its files by name, and their
//! contents. Blocking and CPU-heavy, so run on the background executor;
//! each search spreads over a thread per core.
//!
//! Both start from a [`FileIndex`], every file the tree would list, built
//! once per listing of the tree and searched for every query after: what
//! `git ls-files --cached --others --exclude-standard` would list, found as
//! fd finds files, with a parallel walk by the [`ignore`] crate that skips
//! what Git ignores, plus the tracked files in ignored directories, which
//! the index names.
//!
//! Names match fuzzily, with [`nucleo_matcher`] (Helix's port of fzf's
//! algorithm, tuned for paths). Contents are searched with ripgrep's own
//! libraries, [`grep_searcher`] and [`grep_regex`]: literally unless the
//! query is a regex, ignoring case unless it has a capital (ripgrep's
//! `--smart-case`), skipping binary files, symlinks, and files too large to
//! show.
//!
//! Opening files is most of what a search on disk costs: ripgrep takes
//! seconds on a checkout of 200,000 files on macOS, nearly all of it in the
//! kernel. Reading the same files out of Git's object store is ten times
//! quicker, so a checkout's tracked files that are as the index has them
//! (`git diff-files` lists the rest, from a parallel stat of each) are read
//! from Git, with `git cat-file --batch`, a process per thread, and
//! searched in memory. Only changed and untracked files are read from disk.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::ops::Range;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder};
use ignore::WalkState;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use super::files::{Tracked, Tree, walker};
use super::git::{command, git_bytes};
use crate::diff::TextSnapshot;
use crate::diff::git::MAX_BLOB_BYTES;
use crate::syntax::{Language, LanguageRegistry};

/// Below this many names, matching on one thread is quicker than starting
/// more.
const PARALLEL_THRESHOLD: usize = 20_000;
/// How much of a line before its first match a result shows, in bytes.
const SNIPPET_LEAD: usize = 32;
/// The longest a result's line is shown, in bytes.
const SNIPPET_MAX: usize = 240;
/// Matches highlighted on one line, at most.
const LINE_MATCHES: usize = 32;
/// How long which files are unchanged is trusted for.
const UNCHANGED_FOR: Duration = Duration::from_secs(5);

/// The blobs of a checkout's tracked files that are as the index has them,
/// by path.
type Unchanged = Arc<HashMap<String, String>>;

/// Every file in a tree, by path.
pub struct FileIndex {
    /// `/`-separated paths relative to the root, sorted.
    pub files: Vec<String>,
    /// The languages of the files, the most common first, with how many
    /// files are in each.
    pub languages: Vec<(&'static Language, usize)>,
    /// The tracked files as the index has them, and when that was found,
    /// kept briefly so a query typed a character at a time doesn't stat
    /// every file each time.
    unchanged: Mutex<Option<(Instant, Unchanged)>>,
}

impl FileIndex {
    fn new(mut files: Vec<String>) -> Self {
        files.sort_unstable();
        let mut counts: HashMap<&'static str, (&'static Language, usize)> = HashMap::new();
        for path in &files {
            if let Some(language) = LanguageRegistry::for_path(path) {
                counts.entry(language.name).or_insert((language, 0)).1 += 1;
            }
        }
        let mut languages: Vec<_> = counts.into_values().collect();
        languages.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.name.cmp(b.0.name)));
        Self {
            files,
            languages,
            unchanged: Mutex::new(None),
        }
    }

    /// Readies a content search of `tree`, before there's a query.
    pub fn prepare_contents(&self, tree: &Tree) {
        self.unchanged(&tree.root);
    }

    /// The blobs of the checkout's unchanged files, from the last few
    /// seconds or found again.
    fn unchanged(&self, root: &Path) -> Unchanged {
        let mut cached = self.unchanged.lock().unwrap();
        if let Some((at, unchanged)) = &*cached
            && at.elapsed() < UNCHANGED_FOR
        {
            return unchanged.clone();
        }
        // If Git can't say, everything is read from disk.
        let unchanged = Arc::new(unchanged_blobs(root).unwrap_or_default());
        *cached = Some((Instant::now(), unchanged.clone()));
        unchanged
    }
}

impl Tree {
    /// Every file the tree would list.
    pub fn index(&self) -> Result<FileIndex> {
        Ok(FileIndex::new(disk_files(&self.root, &self.tracked)))
    }
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(16))
}

/// The files on disk under `root` that Git would list: a parallel walk
/// that skips what Git ignores and nested repositories, plus tracked files
/// in ignored directories.
fn disk_files(root: &Path, tracked: &Tracked) -> Vec<String> {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut builder = walker(root);
    builder.threads(threads());
    builder.build_parallel().run(|| {
        let tx = tx.clone();
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            if entry.depth() == 0 {
                return WalkState::Continue;
            }
            let Some(file_type) = entry.file_type() else {
                return WalkState::Continue;
            };
            if file_type.is_dir() {
                // Git lists a nested repository as one entry, not its files.
                return if entry.path().join(".git").exists() {
                    WalkState::Skip
                } else {
                    WalkState::Continue
                };
            }
            if (file_type.is_file() || file_type.is_symlink())
                && let Some(path) = relative(root, entry.path())
            {
                tx.send(path).ok();
            }
            WalkState::Continue
        })
    });
    drop(tx);
    let mut files: Vec<String> = rx.into_iter().collect();
    files.sort_unstable();
    // Tracked files the walk skipped, in ignored directories, if they're
    // still on disk.
    let skipped: Vec<String> = tracked
        .files()
        .iter()
        .filter(|path| files.binary_search(path).is_err())
        .filter(|path| {
            root.join(path)
                .symlink_metadata()
                .is_ok_and(|metadata| !metadata.is_dir())
        })
        .cloned()
        .collect();
    files.extend(skipped);
    files
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?.to_str()?;
    Some(relative.replace('\\', "/"))
}

/// One of the names a fuzzy search matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// Which of the names searched.
    pub index: usize,
    pub score: u32,
    /// Byte offsets of the characters the query matched.
    pub positions: Vec<usize>,
}

/// The best `limit` of `names` for `query`, best first, matched as fzf
/// would: its words in any order, each a fuzzy run of characters, ignoring
/// case unless it has a capital. `paths` scores separators as word
/// boundaries.
pub fn fuzzy_find<S: AsRef<str> + Sync>(
    names: &[S],
    query: &str,
    paths: bool,
    limit: usize,
) -> Vec<FuzzyMatch> {
    let config = if paths {
        Config::DEFAULT.match_paths()
    } else {
        Config::DEFAULT
    };
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let score_chunk = |offset: usize, chunk: &[S]| {
        let mut matcher = Matcher::new(config.clone());
        let mut buf = Vec::new();
        let mut scored = Vec::new();
        for (ix, name) in chunk.iter().enumerate() {
            if let Some(score) = pattern.score(Utf32Str::new(name.as_ref(), &mut buf), &mut matcher)
            {
                scored.push((score, offset + ix));
            }
        }
        scored
    };
    let mut scored = if names.len() < PARALLEL_THRESHOLD {
        score_chunk(0, names)
    } else {
        let size = names.len().div_ceil(threads());
        std::thread::scope(|scope| {
            let workers: Vec<_> = names
                .chunks(size)
                .enumerate()
                .map(|(ix, chunk)| scope.spawn(move || score_chunk(ix * size, chunk)))
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap_or_default())
                .collect()
        })
    };
    // Best score first; then the shortest, which says least besides the
    // match; then in the names' order.
    let order = |a: &(u32, usize), b: &(u32, usize)| {
        b.0.cmp(&a.0)
            .then_with(|| names[a.1].as_ref().len().cmp(&names[b.1].as_ref().len()))
            .then_with(|| a.1.cmp(&b.1))
    };
    if scored.len() > limit {
        scored.select_nth_unstable_by(limit - 1, order);
        scored.truncate(limit);
    }
    scored.sort_unstable_by(order);

    let mut matcher = Matcher::new(config);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    scored
        .into_iter()
        .map(|(score, index)| {
            let name = names[index].as_ref();
            indices.clear();
            pattern.indices(Utf32Str::new(name, &mut buf), &mut matcher, &mut indices);
            indices.sort_unstable();
            indices.dedup();
            FuzzyMatch {
                index,
                score,
                positions: char_offsets(name, &indices),
            }
        })
        .collect()
}

/// Byte offsets of the characters at `chars` (sorted) in `text`.
fn char_offsets(text: &str, chars: &[u32]) -> Vec<usize> {
    let mut wanted = chars.iter().peekable();
    let mut offsets = Vec::with_capacity(chars.len());
    for (ix, (offset, _)) in text.char_indices().enumerate() {
        let Some(&&next) = wanted.peek() else {
            break;
        };
        if ix as u32 == next {
            offsets.push(offset);
            wanted.next();
        }
    }
    offsets
}

/// Byte ranges covering the characters at `positions` (sorted byte
/// offsets) in `text`, adjacent ones joined, for highlighting.
pub fn position_ranges(text: &str, positions: &[usize]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for &start in positions {
        let Some(c) = text.get(start..).and_then(|rest| rest.chars().next()) else {
            continue;
        };
        let end = start + c.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => ranges.push(start..end),
        }
    }
    ranges
}

/// What to look for in files' contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentQuery {
    pub text: String,
    /// A regular expression (Rust's syntax, as ripgrep's), rather than
    /// literal text.
    pub regex: bool,
    /// Match case even when the query is all lowercase.
    pub case_sensitive: bool,
}

#[derive(Debug, Default)]
pub struct ContentResults {
    /// Files with a match, in the index's order.
    pub files: Vec<FileHits>,
    /// Matching lines in all.
    pub lines: usize,
    /// The search stopped at its limit, so there may be more.
    pub truncated: bool,
}

#[derive(Debug)]
pub struct FileHits {
    pub path: String,
    pub lines: Vec<LineHit>,
}

/// A line with a match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineHit {
    /// Zero-based.
    pub line: u32,
    /// The line, or the part of it around its first match, to show.
    pub snippet: String,
    /// The matches in `snippet`.
    pub ranges: Vec<Range<usize>>,
    /// The first match, in the whole line.
    pub first: Range<usize>,
}

/// Searches the contents of `index`'s files in `tree` for `query`,
/// stopping after `limit` matching lines, or once `cancel` is set.
pub fn search_contents(
    tree: &Tree,
    index: &FileIndex,
    query: &ContentQuery,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<ContentResults> {
    let matcher = content_matcher(query)?;
    let search = Search {
        matcher,
        limit,
        cancel,
        found: AtomicUsize::new(0),
        hits: Mutex::new(Vec::new()),
    };
    let unchanged = index.unchanged(&tree.root);
    let (mut from_git, mut from_disk) = (Vec::new(), Vec::new());
    for (ix, path) in index.files.iter().enumerate() {
        match unchanged.get(path.as_str()) {
            Some(oid) => from_git.push((ix, oid.as_str())),
            None => from_disk.push(ix),
        }
    }
    search.run(&tree.root, &from_git, &index.files, &from_disk)?;
    let found = search.found.load(Ordering::Relaxed);
    let mut hits = search.hits.into_inner().unwrap_or_default();
    hits.sort_unstable_by_key(|(ix, _)| *ix);
    // Threads may each have found a line past the limit before stopping.
    let mut lines = 0;
    let mut files = Vec::with_capacity(hits.len());
    for (ix, mut file_lines) in hits {
        if lines >= limit {
            break;
        }
        file_lines.truncate(limit - lines);
        lines += file_lines.len();
        files.push(FileHits {
            path: index.files[ix].clone(),
            lines: file_lines,
        });
    }
    Ok(ContentResults {
        files,
        lines,
        truncated: found >= limit,
    })
}

/// What finds `query`, as ripgrep would.
fn content_matcher(query: &ContentQuery) -> Result<RegexMatcher> {
    RegexMatcherBuilder::new()
        .case_smart(!query.case_sensitive)
        .fixed_strings(!query.regex)
        .line_terminator(Some(b'\n'))
        .build(&query.text)
        .map_err(|err| anyhow!("{err}"))
}

/// Where `query` matches in `text`: each match's line (zero-based) and
/// byte range in it, in order, at most `limit` of them.
pub fn find_in_text(
    text: &TextSnapshot,
    query: &ContentQuery,
    limit: usize,
) -> Result<Vec<(usize, Range<usize>)>> {
    let matcher = content_matcher(query)?;
    let mut found = Vec::new();
    for line in 0..text.line_count() {
        if found.len() >= limit {
            break;
        }
        let _ = matcher.find_iter(text.line(line).as_bytes(), |m| {
            if !m.is_empty() {
                found.push((line as usize, m.start()..m.end()));
            }
            found.len() < limit
        });
    }
    Ok(found)
}

/// A content search in progress, shared by its threads.
struct Search<'a> {
    matcher: RegexMatcher,
    limit: usize,
    cancel: &'a AtomicBool,
    /// Matching lines so far.
    found: AtomicUsize,
    /// Each file's matching lines, by the index's order.
    hits: Mutex<Vec<(usize, Vec<LineHit>)>>,
}

impl Search<'_> {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.found.load(Ordering::Relaxed) >= self.limit
    }

    fn searcher() -> Searcher {
        SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(b'\0'))
            .line_number(true)
            .build()
    }

    /// Searches one file's text with `run`, which feeds lines to the sink
    /// it's given; keeps the lines that match.
    fn file<E>(
        &self,
        ix: usize,
        run: impl FnOnce(
            &RegexMatcher,
            Lossy<&mut dyn FnMut(u64, &str) -> std::io::Result<bool>>,
        ) -> Result<(), E>,
    ) {
        let mut lines = Vec::new();
        let mut sink = |number: u64, line: &str| {
            lines.push(line_hit(&self.matcher, number, line));
            let found = self.found.fetch_add(1, Ordering::Relaxed) + 1;
            Ok(found < self.limit && !self.cancel.load(Ordering::Relaxed))
        };
        // A file that can't be read is left out, as ripgrep would.
        let _ = run(&self.matcher, Lossy(&mut sink));
        if !lines.is_empty() {
            self.hits.lock().unwrap().push((ix, lines));
        }
    }

    /// Searches blobs (by file index) through `git cat-file` in `repo`,
    /// and `from_disk` (indexes into `files`) under `repo` on disk, each
    /// on a thread per core.
    fn run(
        &self,
        repo: &Path,
        from_git: &[(usize, &str)],
        files: &[String],
        from_disk: &[usize],
    ) -> Result<()> {
        let threads = threads();
        let next = AtomicUsize::new(0);
        let failures: Vec<anyhow::Error> = std::thread::scope(|scope| {
            let mut workers = Vec::new();
            if !from_git.is_empty() {
                let size = from_git.len().div_ceil(threads);
                workers.extend(
                    from_git
                        .chunks(size)
                        .map(|chunk| scope.spawn(move || self.blobs(repo, chunk))),
                );
            }
            for _ in 0..threads.min(from_disk.len()) {
                workers.push(scope.spawn(|| {
                    self.disk(repo, files, from_disk, &next);
                    Ok(())
                }));
            }
            workers
                .into_iter()
                .filter_map(|worker| match worker.join() {
                    Ok(result) => result.err(),
                    Err(_) => Some(anyhow!("a search thread panicked")),
                })
                .collect()
        });
        match failures.into_iter().next() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Files on disk: takes the next of `wanted` until there are none.
    fn disk(&self, root: &Path, files: &[String], wanted: &[usize], next: &AtomicUsize) {
        let mut searcher = Self::searcher();
        loop {
            let at = next.fetch_add(1, Ordering::Relaxed);
            let Some(&ix) = wanted.get(at) else {
                break;
            };
            if self.stopped() {
                break;
            }
            let full = root.join(&files[ix]);
            // Not a symlink, as ripgrep would, nor what's too large to
            // show.
            let searched = std::fs::symlink_metadata(&full)
                .is_ok_and(|m| m.is_file() && m.len() <= MAX_BLOB_BYTES);
            if searched {
                self.file(ix, |matcher, sink| {
                    searcher.search_path(matcher, &full, sink)
                });
            }
        }
    }

    /// A run of blobs, through one `git cat-file --batch`.
    fn blobs(&self, repo: &Path, chunk: &[(usize, &str)]) -> Result<()> {
        let mut child = command(repo)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("couldn't run git; is it installed?")?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let input: String = chunk.iter().map(|(_, oid)| format!("{oid}\n")).collect();
        // Written from another thread, so a full stdout can't deadlock.
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut searcher = Self::searcher();
        let mut header = String::new();
        let mut data = Vec::new();
        let mut result = Ok(());
        for &(ix, _) in chunk {
            if self.stopped() {
                break;
            }
            header.clear();
            if let Err(err) = reader.read_line(&mut header) {
                result = Err(err.into());
                break;
            }
            let size = header
                .trim_end()
                .rsplit(' ')
                .next()
                .and_then(|size| size.parse::<u64>().ok());
            let Some(size) = size else {
                result = Err(anyhow!("git couldn't read {}", header.trim_end()));
                break;
            };
            // The object, then a newline.
            if size > MAX_BLOB_BYTES {
                if let Err(err) =
                    std::io::copy(&mut (&mut reader).take(size + 1), &mut std::io::sink())
                {
                    result = Err(err.into());
                    break;
                }
                continue;
            }
            data.clear();
            if let Err(err) = (&mut reader).take(size + 1).read_to_end(&mut data) {
                result = Err(err.into());
                break;
            }
            data.pop();
            self.file(ix, |matcher, sink| {
                searcher.search_slice(matcher, &data, sink)
            });
        }
        // Stopping early leaves git with more to write.
        child.kill().ok();
        child.wait().ok();
        writer.join().ok();
        result
    }
}

/// The blobs of a checkout's tracked files that are on disk as the index
/// has them, by path: the index's regular files, less those `git
/// diff-files` says differ. Symlinks, submodules, and conflicted files
/// are left to the disk.
fn unchanged_blobs(root: &Path) -> Result<HashMap<String, String>> {
    let (staged, changed) = std::thread::scope(|scope| {
        let changed = scope.spawn(|| git_bytes(root, &["diff-files", "-z", "--name-only"]));
        let staged = git_bytes(root, &["ls-files", "-s", "-z"]);
        (staged, changed.join())
    });
    let staged = String::from_utf8(staged?)?;
    let changed = changed.map_err(|_| anyhow!("git diff-files panicked"))??;
    let changed: HashSet<&[u8]> = changed.split(|&b| b == 0).collect();
    Ok(staged
        .split('\0')
        .filter_map(|entry| {
            // `<mode> <oid> <stage>\t<path>`
            let (meta, path) = entry.split_once('\t')?;
            let mut meta = meta.split(' ');
            let (mode, oid, stage) = (meta.next()?, meta.next()?, meta.next()?);
            let regular = matches!(mode, "100644" | "100755");
            (regular && stage == "0" && !changed.contains(path.as_bytes()))
                .then(|| (path.to_owned(), oid.to_owned()))
        })
        .collect())
}

/// A matching line, as a result shows it.
fn line_hit(matcher: &RegexMatcher, number: u64, line: &str) -> LineHit {
    let line = line.trim_end_matches(['\n', '\r']);
    let mut ranges = Vec::new();
    let _ = matcher.find_iter(line.as_bytes(), |m| {
        if !m.is_empty() {
            ranges.push(m.start()..m.end());
        }
        ranges.len() < LINE_MATCHES
    });
    let first = ranges.first().cloned().unwrap_or(0..0);

    // From the indentation, or a little before the first match if it's
    // far along.
    let mut start = line.len() - line.trim_start().len();
    let mut snippet = String::new();
    if first.start > start + SNIPPET_LEAD * 2 {
        start = floor_boundary(line, first.start - SNIPPET_LEAD);
        snippet.push('…');
    }
    let end = floor_boundary(line, (start + SNIPPET_MAX).min(line.len()));
    let shift = snippet.len();
    snippet.push_str(&line[start..end]);
    if end < line.len() {
        snippet.push('…');
    }
    let ranges = ranges
        .into_iter()
        .map(|r| (r.start.max(start) - start + shift)..(r.end.min(end).max(start) - start + shift))
        .filter(|r| !r.is_empty())
        .collect();
    LineHit {
        line: number.saturating_sub(1) as u32,
        snippet,
        ranges,
        first,
    }
}

fn floor_boundary(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;
    use crate::projects::files::Tree;
    use crate::projects::git::git;

    fn names(matches: &[FuzzyMatch], names: &[&str]) -> Vec<String> {
        matches.iter().map(|m| names[m.index].to_owned()).collect()
    }

    #[test]
    fn finds_names_fuzzily_best_first() {
        let paths = [
            "src/ui/file_browser/mod.rs",
            "src/state/browse.rs",
            "README.md",
            "src/ui/workspace.rs",
            "assets/icons/folder.svg",
        ];
        let found = fuzzy_find(&paths, "brows", true, 10);
        assert_eq!(
            names(&found, &paths),
            ["src/state/browse.rs", "src/ui/file_browser/mod.rs"]
        );
        // Words match in any order.
        let found = fuzzy_find(&paths, "mod file", true, 10);
        assert_eq!(names(&found, &paths), ["src/ui/file_browser/mod.rs"]);
        // A capital matches case.
        assert!(fuzzy_find(&paths, "readme", true, 10).len() == 1);
        assert!(fuzzy_find(&paths, "ReadMe", true, 10).is_empty());
        assert!(fuzzy_find(&paths, "  ", true, 10).is_empty());
        assert_eq!(fuzzy_find(&paths, "s", true, 2).len(), 2);

        let found = fuzzy_find(&["src/ünï.rs"], "ïrs", true, 1);
        assert_eq!(found[0].positions, [7, 10, 11]);
        assert_eq!(
            position_ranges("src/ünï.rs", &found[0].positions),
            [7..9, 10..12]
        );
    }

    #[test]
    fn finds_names_in_parallel_like_on_one_thread() {
        let paths: Vec<String> = (0..PARALLEL_THRESHOLD * 2)
            .map(|ix| format!("src/module_{ix}/file.rs"))
            .collect();
        let found = fuzzy_find(&paths, "module_3999file", true, 3);
        assert_eq!(paths[found[0].index], "src/module_3999/file.rs");
        assert_eq!(found.len(), 3);
    }

    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, contents: &[u8]| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        };
        git(root, &["init", "-q", "-b", "main"]).unwrap();
        write(".gitignore", b"target/\n");
        write("src/lib.rs", b"fn widget() {}\nfn Widget() {}\n");
        write("src/ui/view.rs", b"// draws the widget\n");
        write(
            "target/kept.rs",
            b"fn widget() {} // tracked though ignored\n",
        );
        write("logo.png", b"\x89PNG\0widget");
        git(root, &["add", "-A"]).unwrap();
        git(root, &["add", "-f", "target/kept.rs"]).unwrap();
        let status = command(root)
            .args(["-c", "user.email=me@example.com", "-c", "user.name=Me"])
            .args(["commit", "-q", "-m", "Add files"])
            .status()
            .unwrap();
        assert!(status.success());
        write("notes.md", b"widget notes, untracked\n");
        // Changed on disk since the commit.
        write("src/ui/view.rs", b"// draws the gadget\n");
        write("target/out.rs", b"fn widget() {} // ignored\n");
        std::fs::create_dir_all(root.join("vendor/lib")).unwrap();
        git(&root.join("vendor/lib"), &["init", "-q"]).unwrap();
        write("vendor/lib/widget.rs", b"fn widget() {}\n");
        dir
    }

    fn found(results: &ContentResults) -> Vec<(String, u32, String)> {
        results
            .files
            .iter()
            .flat_map(|file| {
                file.lines
                    .iter()
                    .map(|hit| (file.path.clone(), hit.line, hit.snippet.clone()))
            })
            .collect()
    }

    #[test]
    fn indexes_and_searches_a_checkout() {
        let dir = checkout();
        let root = dir.path();
        let tree = Tree::read(root.to_owned()).unwrap();
        let index = tree.index().unwrap();
        assert_eq!(
            index.files,
            [
                ".gitignore",
                "logo.png",
                "notes.md",
                "src/lib.rs",
                "src/ui/view.rs",
                "target/kept.rs",
            ]
        );
        assert_eq!(index.languages[0].0.name, "Rust");
        assert_eq!(index.languages[0].1, 3);

        let query = |text: &str| ContentQuery {
            text: text.to_owned(),
            regex: false,
            case_sensitive: false,
        };
        let cancel = AtomicBool::new(false);
        let results = search_contents(&tree, &index, &query("widget"), 100, &cancel).unwrap();
        assert_eq!(
            found(&results),
            [
                ("notes.md".into(), 0, "widget notes, untracked".into()),
                ("src/lib.rs".into(), 0, "fn widget() {}".into()),
                ("src/lib.rs".into(), 1, "fn Widget() {}".into()),
                (
                    "target/kept.rs".into(),
                    0,
                    "fn widget() {} // tracked though ignored".into()
                ),
            ]
        );
        assert_eq!(results.files[1].lines[0].ranges, [3..9]);
        assert_eq!(results.lines, 4);
        let results = search_contents(&tree, &index, &query("gadget"), 100, &cancel).unwrap();
        assert_eq!(
            found(&results),
            [("src/ui/view.rs".into(), 0, "// draws the gadget".into())]
        );
        assert!(!results.truncated);

        // A capital matches case; so does asking to.
        let results = search_contents(&tree, &index, &query("Widget"), 100, &cancel).unwrap();
        assert_eq!(found(&results).len(), 1);
        let sensitive = ContentQuery {
            case_sensitive: true,
            ..query("widget")
        };
        let results = search_contents(&tree, &index, &sensitive, 100, &cancel).unwrap();
        assert_eq!(found(&results).len(), 3);

        let regex = ContentQuery {
            regex: true,
            ..query(r"fn \w+\(")
        };
        let results = search_contents(&tree, &index, &regex, 100, &cancel).unwrap();
        assert_eq!(found(&results).len(), 3);
        assert!(search_contents(&tree, &index, &regex, 100, &cancel).is_ok());
        let literal = query(r"fn \w+\(");
        let results = search_contents(&tree, &index, &literal, 100, &cancel).unwrap();
        assert!(results.files.is_empty());
        let bad = ContentQuery {
            regex: true,
            ..query("fn (")
        };
        assert!(search_contents(&tree, &index, &bad, 100, &cancel).is_err());

        let results = search_contents(&tree, &index, &query("widget"), 2, &cancel).unwrap();
        assert_eq!(results.lines, 2);
        assert!(results.truncated);
    }

    #[test]
    fn finds_matches_in_a_file() {
        let text = TextSnapshot::new(b"One one\ntwo\n  one\n".to_vec(), "notes.txt");
        let query = |text: &str, regex| ContentQuery {
            text: text.into(),
            regex,
            case_sensitive: false,
        };
        assert_eq!(
            find_in_text(&text, &query("one", false), 100).unwrap(),
            [(0, 0..3), (0, 4..7), (2, 2..5)]
        );
        assert_eq!(
            find_in_text(&text, &query("One", false), 100).unwrap(),
            [(0, 0..3)]
        );
        assert_eq!(
            find_in_text(&text, &query("one", false), 2).unwrap().len(),
            2
        );
        assert_eq!(
            find_in_text(&text, &query(r"^\s+\w", true), 100).unwrap(),
            [(2, 0..3)]
        );
        assert!(find_in_text(&text, &query("(", true), 100).is_err());
    }

    #[test]
    fn shows_the_part_of_a_long_line_with_the_match() {
        let matcher = RegexMatcherBuilder::new()
            .fixed_strings(true)
            .build("needle")
            .unwrap();
        let hit = line_hit(&matcher, 3, "    short needle\n");
        assert_eq!(hit.line, 2);
        assert_eq!(hit.snippet, "short needle");
        assert_eq!(hit.ranges, [6..12]);
        assert_eq!(hit.first, 10..16);

        let line = format!("{}needle{}", "é".repeat(100), "x".repeat(400));
        let hit = line_hit(&matcher, 1, &line);
        assert!(hit.snippet.starts_with('…') && hit.snippet.ends_with('…'));
        assert_eq!(&hit.snippet[hit.ranges[0].clone()], "needle");
        assert_eq!(&line[hit.first.clone()], "needle");
    }

    /// Times indexing and searching a large checkout, on disk and at its
    /// HEAD: `DOCKET_SEARCH_BENCH=<checkout> cargo test --release --
    /// --ignored search_benchmark --nocapture`.
    #[test]
    #[ignore = "needs a large checkout"]
    fn search_benchmark() {
        let root = std::path::PathBuf::from(
            std::env::var("DOCKET_SEARCH_BENCH").expect("DOCKET_SEARCH_BENCH is a checkout"),
        );
        let timed = |label: &str, run: &mut dyn FnMut() -> String| {
            let start = std::time::Instant::now();
            let what = run();
            eprintln!("{label:<34} {:>8.1?}  {what}", start.elapsed());
        };
        let tree = &Tree::read(root.clone()).unwrap();
        let name = "disk";
        let cancel = AtomicBool::new(false);
        timed("disk: unchanged blobs", &mut || {
            format!("{} unchanged", unchanged_blobs(&root).unwrap().len())
        });
        {
            let mut index = None;
            timed(&format!("{name}: index"), &mut || {
                let built = tree.index().unwrap();
                let what = format!("{} files", built.files.len());
                index = Some(built);
                what
            });
            let index = index.unwrap();
            for query in ["a", "button", "src/comp/modal"] {
                timed(&format!("{name}: files {query:?}"), &mut || {
                    let found = fuzzy_find(&index.files, query, true, 200);
                    format!(
                        "{} shown, best {:?}",
                        found.len(),
                        found.first().map(|f| &index.files[f.index])
                    )
                });
            }
            for (text, regex) in [
                ("useState", false),
                ("TODO\\(\\w+\\)", true),
                ("zzqqxxnotfound", false),
            ] {
                let query = ContentQuery {
                    text: text.into(),
                    regex,
                    case_sensitive: false,
                };
                timed(&format!("{name}: contents {text:?}"), &mut || {
                    let found = search_contents(tree, &index, &query, 2_000, &cancel).unwrap();
                    format!(
                        "{} lines in {} files, truncated {}",
                        found.lines,
                        found.files.len(),
                        found.truncated
                    )
                });
            }
        }
    }
}
