//! Loads a pull request's diff from the project's local checkout. Shells
//! out to `git` (blocking), so run on the background executor.
//!
//! The PR's commits are fetched into `refs/docket/pull/<n>/{head,base}`
//! when they aren't already local. That namespace keeps the user's branches
//! and `FETCH_HEAD` untouched, and keeps the objects from being collected.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context as _, Result, anyhow, bail};

use super::{FileContent, FileDiff, FileStatus, PullRequestDiff, TextContent, TextSnapshot};
use crate::github::RepoRef;
use crate::projects::Checkout;
use crate::projects::git::{command, git};

/// Larger files show a notice instead of their contents.
pub(crate) const MAX_BLOB_BYTES: u64 = 5 * 1024 * 1024;
/// Git's own heuristic: a NUL in the first 8000 bytes means binary.
pub(crate) const BINARY_SNIFF_BYTES: usize = 8000;
const SUBMODULE_MODE: &str = "160000";

/// What's needed to find a PR's commits.
#[derive(Clone, Debug)]
pub struct PullRequestRefs {
    pub number: u64,
    pub base_ref_name: String,
    pub base_oid: String,
    pub head_oid: String,
}

/// The remote to fetch `repository`'s PRs from: the checkout's remote for
/// it, or its GitHub URL. PR refs live on the base repository, so this works
/// for PRs from forks too.
pub fn remote_for(checkout: &Checkout, repository: &RepoRef) -> String {
    checkout
        .remotes
        .iter()
        .find(|remote| remote.repository == *repository)
        .map(|remote| remote.name.clone())
        .unwrap_or_else(|| format!("https://github.com/{repository}.git"))
}

/// Loads the diff GitHub shows for a PR: from the merge base of its base
/// and head to its head.
pub fn load(root: &Path, remote: &str, pr: &PullRequestRefs) -> Result<PullRequestDiff> {
    let (base_oid, head_oid) = resolve_commits(root, remote, pr)?;
    load_branch(root, &base_oid, &head_oid)
}

/// Loads what the commit `head_oid` changes since it left `base_oid`, as a
/// pull request from one into the other would show: from their merge base
/// to the head. Both must be local.
pub fn load_branch(root: &Path, base_oid: &str, head_oid: &str) -> Result<PullRequestDiff> {
    let merge_base = git(root, &["merge-base", base_oid, head_oid])
        .context("the branches share no history")?
        .trim()
        .to_owned();
    let head_oid = head_oid.to_owned();

    let changes = changed_files(root, &merge_base, &head_oid)?;
    let oids: Vec<&str> = changes
        .iter()
        .filter(|change| !change.is_submodule())
        .flat_map(|change| [&change.old_oid, &change.new_oid])
        .filter(|oid| !is_null_oid(oid))
        .map(String::as_str)
        .collect();
    let blobs = read_blobs(root, &oids)?;

    let files = changes
        .into_iter()
        .map(|change| file_diff(change, &blobs))
        .collect();
    Ok(PullRequestDiff { head_oid, files })
}

/// The PR's base and head commits, fetching them if they aren't local.
/// Prefers the commits GitHub reported, so the diff matches the PR as the
/// docket showed it.
fn resolve_commits(root: &Path, remote: &str, pr: &PullRequestRefs) -> Result<(String, String)> {
    let head_ref = format!("refs/docket/pull/{}/head", pr.number);
    let base_ref = format!("refs/docket/pull/{}/base", pr.number);
    // Only fetch what's missing: in a large repository, an already-local
    // head (say, your own PR) saves most of the transfer.
    let mut refspecs = Vec::new();
    if !has_commit(root, &pr.head_oid) {
        refspecs.push(format!("+refs/pull/{}/head:{head_ref}", pr.number));
    }
    if !has_commit(root, &pr.base_oid) {
        refspecs.push(format!("+refs/heads/{}:{base_ref}", pr.base_ref_name));
    }
    if refspecs.is_empty() {
        return Ok((pr.base_oid.clone(), pr.head_oid.clone()));
    }

    let mut args = vec![
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-write-fetch-head",
        "--no-recurse-submodules",
        remote,
    ];
    args.extend(refspecs.iter().map(String::as_str));
    git(root, &args).with_context(|| format!("couldn't fetch the pull request from {remote}"))?;

    // The PR may have moved since the docket loaded; fall back to what was
    // fetched.
    let pick = |oid: &str, fetched: &str| -> Result<String> {
        if has_commit(root, oid) {
            return Ok(oid.to_owned());
        }
        Ok(git(root, &["rev-parse", "--verify", "--quiet", fetched])?
            .trim()
            .to_owned())
    };
    Ok((
        pick(&pr.base_oid, &base_ref)?,
        pick(&pr.head_oid, &head_ref)?,
    ))
}

fn has_commit(root: &Path, oid: &str) -> bool {
    !oid.is_empty() && git(root, &["cat-file", "-e", &format!("{oid}^{{commit}}")]).is_ok()
}

fn is_null_oid(oid: &str) -> bool {
    oid.bytes().all(|b| b == b'0')
}

/// One entry of `git diff --raw`.
#[derive(Debug)]
struct RawChange {
    old_mode: String,
    new_mode: String,
    old_oid: String,
    new_oid: String,
    status: FileStatus,
    old_path: Option<String>,
    new_path: Option<String>,
}

impl RawChange {
    fn is_submodule(&self) -> bool {
        self.old_mode == SUBMODULE_MODE || self.new_mode == SUBMODULE_MODE
    }
}

fn changed_files(root: &Path, from: &str, to: &str) -> Result<Vec<RawChange>> {
    let output = git(
        root,
        &[
            "diff",
            "--raw",
            "-z",
            "-M",
            "--no-abbrev",
            "--no-relative",
            "--no-ext-diff",
            "--no-textconv",
            from,
            to,
        ],
    )?;
    parse_raw(&output)
}

/// Parses `git diff --raw -z`: `:<old mode> <new mode> <old oid> <new oid>
/// <status>`, then one path, or two for renames and copies, NUL-separated.
fn parse_raw(output: &str) -> Result<Vec<RawChange>> {
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    let mut changes = Vec::new();
    while let Some(header) = fields.next() {
        let header = header
            .strip_prefix(':')
            .ok_or_else(|| anyhow!("unexpected git diff output: {header:?}"))?;
        let [old_mode, new_mode, old_oid, new_oid, status] = header
            .split(' ')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| anyhow!("unexpected git diff header: {header:?}"))?;
        let mut path = || {
            fields
                .next()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("git diff entry is missing its path"))
        };
        let similarity = || status[1..].parse().unwrap_or(0);
        let (status, old_path, new_path) = match status.as_bytes().first() {
            Some(b'A') => (FileStatus::Added, None, Some(path()?)),
            Some(b'D') => (FileStatus::Deleted, Some(path()?), None),
            Some(b'R') => (
                FileStatus::Renamed {
                    similarity: similarity(),
                },
                Some(path()?),
                Some(path()?),
            ),
            Some(b'C') => (
                FileStatus::Copied {
                    similarity: similarity(),
                },
                Some(path()?),
                Some(path()?),
            ),
            Some(b'T') => {
                let path = path()?;
                (FileStatus::TypeChanged, Some(path.clone()), Some(path))
            }
            // `M`, and statuses a commit-to-commit diff doesn't produce.
            _ => {
                let path = path()?;
                (FileStatus::Modified, Some(path.clone()), Some(path))
            }
        };
        changes.push(RawChange {
            old_mode: old_mode.to_owned(),
            new_mode: new_mode.to_owned(),
            old_oid: old_oid.to_owned(),
            new_oid: new_oid.to_owned(),
            status,
            old_path,
            new_path,
        });
    }
    Ok(changes)
}

enum Blob {
    Data(Vec<u8>),
    TooLarge(u64),
}

/// Reads blobs through one `git cat-file --batch` process.
fn read_blobs(root: &Path, oids: &[&str]) -> Result<HashMap<String, Blob>> {
    if oids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut child = command(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run git; is it installed?")?;

    // Write on another thread so a full stdout pipe can't deadlock us.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input: String = oids.iter().map(|oid| format!("{oid}\n")).collect();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));

    let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let mut blobs = HashMap::new();
    let mut header = String::new();
    for _ in oids {
        header.clear();
        reader.read_line(&mut header)?;
        let mut parts = header.trim_end().split(' ');
        let (Some(oid), Some(kind), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            bail!("git couldn't read object: {}", header.trim_end());
        };
        if kind != "blob" {
            bail!("expected a blob, got a {kind}: {oid}");
        }
        let size: u64 = size.parse()?;
        let blob = if size > MAX_BLOB_BYTES {
            io::copy(&mut (&mut reader).take(size), &mut io::sink())?;
            Blob::TooLarge(size)
        } else {
            let mut data = vec![0; size as usize];
            reader.read_exact(&mut data)?;
            Blob::Data(data)
        };
        // Each object is followed by a newline.
        reader.read_exact(&mut [0])?;
        blobs.insert(oid.to_owned(), blob);
    }

    writer
        .join()
        .map_err(|_| anyhow!("git cat-file writer panicked"))??;
    let status = child.wait()?;
    if !status.success() {
        bail!("git cat-file failed");
    }
    Ok(blobs)
}

fn file_diff(change: RawChange, blobs: &HashMap<String, Blob>) -> FileDiff {
    let content = if change.is_submodule() {
        FileContent::Submodule
    } else {
        let take = |oid: &str| {
            if is_null_oid(oid) {
                None
            } else {
                // Identical blobs (a pure rename) share one entry.
                match blobs.get(oid) {
                    Some(Blob::Data(data)) => Some(Blob::Data(data.clone())),
                    Some(Blob::TooLarge(size)) => Some(Blob::TooLarge(*size)),
                    None => Some(Blob::Data(Vec::new())),
                }
            }
        };
        let (old, new) = (take(&change.old_oid), take(&change.new_oid));
        text_content(old, new, &change)
    };

    let (additions, deletions) = match &content {
        FileContent::Text(text) => text.stats(),
        _ => (0, 0),
    };
    FileDiff {
        old_path: change.old_path,
        new_path: change.new_path,
        status: change.status,
        additions,
        deletions,
        content,
    }
}

fn text_content(old: Option<Blob>, new: Option<Blob>, change: &RawChange) -> FileContent {
    let mut too_large = None;
    let mut binary = false;
    let mut data = |blob: Option<Blob>| match blob {
        Some(Blob::Data(data)) => {
            binary |= data[..data.len().min(BINARY_SNIFF_BYTES)].contains(&0);
            Some(data)
        }
        Some(Blob::TooLarge(size)) => {
            too_large = Some(too_large.unwrap_or(0).max(size));
            None
        }
        None => None,
    };
    let (old, new) = (data(old), data(new));
    if let Some(bytes) = too_large {
        return FileContent::TooLarge { bytes };
    }
    if binary {
        return FileContent::Binary;
    }
    let snapshot = |data: Option<Vec<u8>>, path: &Option<String>| {
        data.map(|data| TextSnapshot::new(data, path.as_deref().unwrap_or_default()))
    };
    FileContent::Text(TextContent::new(
        snapshot(old, &change.old_path),
        snapshot(new, &change.new_path),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raw_diff() {
        let oid = |c: char| c.to_string().repeat(40);
        let null = oid('0');
        let output = format!(
            ":000000 100644 {null} {a} A\0added.rs\0\
             :100644 100644 {b} {c} R087\0old.rs\0new.rs\0\
             :160000 160000 {d} {e} M\0vendor/lib\0",
            a = oid('a'),
            b = oid('b'),
            c = oid('c'),
            d = oid('d'),
            e = oid('e'),
        );
        let changes = parse_raw(&output).unwrap();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].status, FileStatus::Added);
        assert_eq!(changes[0].old_path, None);
        assert_eq!(changes[0].new_path.as_deref(), Some("added.rs"));
        assert_eq!(changes[1].status, FileStatus::Renamed { similarity: 87 });
        assert_eq!(changes[1].old_path.as_deref(), Some("old.rs"));
        assert_eq!(changes[1].new_path.as_deref(), Some("new.rs"));
        assert!(changes[2].is_submodule());
    }

    /// A "GitHub" repository with PR #1, and a clone of it as the project.
    #[test]
    fn loads_pull_request_from_remote() {
        let dir = std::env::temp_dir().join(format!("docket-diff-{}", std::process::id()));
        let (upstream, checkout) = (dir.join("upstream"), dir.join("checkout"));
        std::fs::create_dir_all(&upstream).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let mut full = vec![
                "-c",
                "user.name=Docket",
                "-c",
                "user.email=docket@example.com",
                "-c",
                "commit.gpgsign=false",
            ];
            full.extend_from_slice(args);
            git(dir, &full).unwrap().trim().to_owned()
        };
        let write = |path: &str, contents: &[u8]| {
            let path = upstream.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        };

        run(&upstream, &["init", "-q", "-b", "main"]);
        write("src/lib.rs", b"fn one() {}\n\nfn two() {}\n");
        write("old_name.txt", b"same\ncontent\n");
        write("deleted.txt", b"bye\n");
        run(&upstream, &["add", "-A"]);
        run(&upstream, &["commit", "-q", "-m", "base"]);

        run(&upstream, &["checkout", "-q", "-b", "feature"]);
        write("src/lib.rs", b"fn one() {}\n\nfn three() {}\n");
        std::fs::rename(upstream.join("old_name.txt"), upstream.join("new_name.txt")).unwrap();
        std::fs::remove_file(upstream.join("deleted.txt")).unwrap();
        write("added.md", b"# Hi\n");
        write("image.bin", b"\x89PNG\0\0\0");
        run(&upstream, &["add", "-A"]);
        run(&upstream, &["commit", "-q", "-m", "feature"]);
        let head_oid = run(&upstream, &["rev-parse", "HEAD"]);
        run(&upstream, &["update-ref", "refs/pull/1/head", &head_oid]);

        // The base branch moves on after the PR branched off; the diff
        // should still only show the PR's changes.
        run(&upstream, &["checkout", "-q", "main"]);
        run(&upstream, &["branch", "-q", "-D", "feature"]);
        write("unrelated.txt", b"later\n");
        run(&upstream, &["add", "-A"]);
        run(&upstream, &["commit", "-q", "-m", "later"]);
        let base_oid = run(&upstream, &["rev-parse", "HEAD"]);

        // `--no-local` so the clone only gets reachable objects, as from
        // GitHub, rather than a copy of the whole object store.
        run(
            &dir,
            &[
                "clone",
                "-q",
                "--no-local",
                upstream.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        let pr = PullRequestRefs {
            number: 1,
            base_ref_name: "main".into(),
            base_oid,
            head_oid: head_oid.clone(),
        };
        assert!(!has_commit(&checkout, &head_oid));

        let diff = load(&checkout, "origin", &pr).unwrap();
        assert_eq!(diff.head_oid, head_oid);
        let summary: Vec<_> = diff
            .files
            .iter()
            .map(|file| (file.path(), file.status, file.additions, file.deletions))
            .collect();
        assert_eq!(
            summary,
            [
                ("added.md", FileStatus::Added, 1, 0),
                ("deleted.txt", FileStatus::Deleted, 0, 1),
                ("image.bin", FileStatus::Added, 0, 0),
                (
                    "new_name.txt",
                    FileStatus::Renamed { similarity: 100 },
                    0,
                    0
                ),
                ("src/lib.rs", FileStatus::Modified, 1, 1),
            ]
        );
        assert!(matches!(diff.files[2].content, FileContent::Binary));
        let FileContent::Text(lib) = &diff.files[4].content else {
            panic!("src/lib.rs should be text");
        };
        assert_eq!(lib.new.as_ref().unwrap().line(2), "fn three() {}");
        assert_eq!(lib.new.as_ref().unwrap().language.unwrap().name, "Rust");

        // Fetched into Docket's namespace, without touching FETCH_HEAD.
        assert_eq!(
            run(&checkout, &["rev-parse", "refs/docket/pull/1/head"]),
            head_oid
        );
        assert!(!checkout.join(".git/FETCH_HEAD").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
