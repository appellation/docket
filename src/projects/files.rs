//! The files in a project's checkout as they are on disk, for browsing.
//! Shells out to `git` and reads the disk (blocking), so run on the
//! background executor.
//!
//! Directories are listed one at a time, as they're opened, so a large
//! checkout isn't walked up front. A directory shows what `git ls-files
//! --cached --others --exclude-standard` would: tracked entries still on
//! disk, plus untracked ones that aren't ignored. So `target/` and
//! `node_modules/` stay out, but a tracked file in an ignored directory
//! doesn't.
//!
//! Git's index says what's tracked. Whether an untracked entry is ignored
//! comes from the `ignore` crate (ripgrep's and fd's), which reads Git's
//! ignore files itself, so expanding a directory doesn't start a process.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use ignore::WalkBuilder;

use super::git::git;
use crate::diff::TextSnapshot;
use crate::diff::git::{BINARY_SNIFF_BYTES, MAX_BLOB_BYTES};

/// What Git tracks: every path in the index, and every directory one is
/// in. Reading the index is cheap even for a large checkout; it's finding
/// untracked files and deleted ones that takes walking the whole tree.
pub struct Tracked {
    paths: HashSet<String>,
}

impl Tracked {
    /// From `/`-separated paths of files.
    pub fn from_paths<'a>(files: impl IntoIterator<Item = &'a str>) -> Self {
        let mut paths = HashSet::new();
        for file in files {
            let mut path = file;
            // Stop at the first directory already seen: its own are too.
            while paths.insert(path.to_owned()) {
                match path.rsplit_once('/') {
                    Some((dir, _)) => path = dir,
                    None => break,
                }
            }
        }
        Self { paths }
    }

    fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }
}

/// Reads the index.
pub fn tracked(root: &Path) -> Result<Tracked> {
    let listed = git(root, &["ls-files", "-z", "--cached"])?;
    Ok(Tracked::from_paths(
        listed.split('\0').filter(|path| !path.is_empty()),
    ))
}

/// The checked-out branch, or the commit when detached. `None` before the
/// first commit.
pub fn head(root: &Path) -> Option<String> {
    let head = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    match head.trim() {
        "HEAD" => Some(
            git(root, &["rev-parse", "--short", "HEAD"])
                .ok()?
                .trim()
                .to_owned(),
        ),
        branch => Some(branch.to_owned()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    /// Relative to the checkout's root, `/`-separated.
    pub path: String,
    pub kind: EntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A submodule or nested repository. Git treats it as one entry, so
    /// it isn't listed into.
    Repository,
}

/// The entries in `dir` (`""` for the root) that Git would list,
/// directories first, then by name.
pub fn list_dir(root: &Path, dir: &str, tracked: &Tracked) -> Result<Vec<DirEntry>> {
    let full = root.join(dir);
    let read = std::fs::read_dir(&full).with_context(|| match dir {
        "" => format!("couldn't read {}", root.display()),
        dir => format!("couldn't read {dir}"),
    })?;
    let mut entries = Vec::new();
    for entry in read {
        let entry = entry.with_context(|| format!("couldn't read {}", full.display()))?;
        // Git can't track a name that isn't UTF-8 here, so skip it rather
        // than show a path that can't be opened.
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name == ".git" {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Git tracks a symlink as a file, whatever it points at.
        let kind = if file_type.is_dir() {
            if entry.path().join(".git").exists() {
                EntryKind::Repository
            } else {
                EntryKind::Dir
            }
        } else if file_type.is_file() || file_type.is_symlink() {
            EntryKind::File
        } else {
            continue;
        };
        let path = match dir {
            "" => name.clone(),
            dir => format!("{dir}/{name}"),
        };
        entries.push(DirEntry { name, path, kind });
    }

    // Only what Git doesn't track can be ignored. An untracked directory
    // shows, as in `git ls-files`, only if there's something in it to list.
    if entries.iter().any(|entry| !tracked.contains(&entry.path)) {
        let unignored = unignored(&full);
        entries.retain(|entry| {
            tracked.contains(&entry.path)
                || (unignored.contains(&entry.name)
                    && (entry.kind != EntryKind::Dir || has_unignored(&root.join(&entry.path))))
        });
    }

    entries.sort_by(|a, b| {
        (b.kind == EntryKind::Dir)
            .cmp(&(a.kind == EntryKind::Dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

/// A walk that skips what Git ignores, by the `.gitignore`s from the
/// repository's root down, its `info/exclude`, and the user's global
/// excludes. Nothing else ripgrep or fd would skip (hidden files,
/// `.ignore`) is skipped.
fn walker(path: &Path) -> WalkBuilder {
    let mut walker = WalkBuilder::new(path);
    walker
        .standard_filters(false)
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(|entry| entry.file_name() != ".git");
    walker
}

/// Names of the entries directly in `dir` that Git doesn't ignore.
fn unignored(dir: &Path) -> HashSet<String> {
    walker(dir)
        .max_depth(Some(1))
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() == 1)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect()
}

/// Whether Git would list anything in the untracked directory `dir`: a
/// file it doesn't ignore, or a nested repository. A cache directory whose
/// own `.gitignore` ignores all of it has nothing. Stops at the first.
fn has_unignored(dir: &Path) -> bool {
    walker(dir)
        .build()
        .filter_map(Result::ok)
        .skip(1)
        .any(|entry| match entry.file_type() {
            Some(file_type) if file_type.is_dir() => entry.path().join(".git").exists(),
            Some(_) => true,
            None => false,
        })
}

/// A file's contents as read from the checkout.
pub enum LocalFile {
    Text(TextSnapshot),
    Binary,
    TooLarge {
        bytes: u64,
    },
    /// A submodule (or nested repository), which Git lists as one entry.
    Directory,
}

/// Reads `path`, relative to the checkout's root.
pub fn read(root: &Path, path: &str) -> Result<LocalFile> {
    let full = root.join(path);
    let metadata = std::fs::metadata(&full).with_context(|| format!("couldn't read {path}"))?;
    if metadata.is_dir() {
        return Ok(LocalFile::Directory);
    }
    if !metadata.is_file() {
        bail!("{path} isn't a regular file");
    }
    if metadata.len() > MAX_BLOB_BYTES {
        return Ok(LocalFile::TooLarge {
            bytes: metadata.len(),
        });
    }
    let mut data = Vec::with_capacity(metadata.len() as usize);
    // The file can grow between the check and the read.
    std::fs::File::open(&full)
        .and_then(|file| file.take(MAX_BLOB_BYTES + 1).read_to_end(&mut data))
        .with_context(|| format!("couldn't read {path}"))?;
    if data.len() as u64 > MAX_BLOB_BYTES {
        return Ok(LocalFile::TooLarge {
            bytes: data.len() as u64,
        });
    }
    if data[..data.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return Ok(LocalFile::Binary);
    }
    Ok(LocalFile::Text(TextSnapshot::new(data, path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_files_and_their_directories() {
        let tracked = Tracked::from_paths(["src/ui/view.rs", "src/main.rs", "README.md"]);
        for path in [
            "src",
            "src/ui",
            "src/ui/view.rs",
            "src/main.rs",
            "README.md",
        ] {
            assert!(tracked.contains(path), "{path}");
        }
        assert!(!tracked.contains("src/u"));
        assert!(!tracked.contains("docs"));
    }

    #[test]
    fn lists_and_reads_checkout() {
        let dir = std::env::temp_dir().join(format!("docket-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |path: &str, contents: &[u8]| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        };
        git(&dir, &["init", "-q", "-b", "main"]).unwrap();
        write(".gitignore", b"target/\n*.log\n");
        write("src/lib.rs", b"fn one() {}\n");
        write("src/B.rs", b"");
        write("gone.txt", b"bye\n");
        write("build/kept.log", b"tracked though ignored\n");
        git(&dir, &["add", "-A"]).unwrap();
        git(&dir, &["add", "-f", "build/kept.log"]).unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        write("notes.md", b"# Untracked\n");
        write("debug.log", b"ignored");
        write("target/debug/out", b"ignored");
        std::fs::create_dir_all(dir.join("vendor/lib")).unwrap();
        git(&dir.join("vendor/lib"), &["init", "-q"]).unwrap();
        write(".cache/.gitignore", b"*\n");
        write(".cache/data", b"ignored by its own .gitignore");
        write("src/[id].rs", b"");
        write(".git/info/exclude", b"secret.txt\n");
        write("secret.txt", b"excluded locally");
        write("logo.png", b"\x89PNG\0\0\0");

        let tracked = tracked(&dir).unwrap();
        let names = |path: &str| -> Vec<(String, EntryKind)> {
            list_dir(&dir, path, &tracked)
                .unwrap()
                .into_iter()
                .map(|entry| (entry.name, entry.kind))
                .collect()
        };
        let file = |name: &str| (name.to_owned(), EntryKind::File);
        let dir_entry = |name: &str| (name.to_owned(), EntryKind::Dir);
        assert_eq!(
            names(""),
            [
                dir_entry("build"),
                dir_entry("src"),
                dir_entry("vendor"),
                file(".gitignore"),
                file("logo.png"),
                file("notes.md"),
            ]
        );
        assert_eq!(
            names("src"),
            [file("[id].rs"), file("B.rs"), file("lib.rs")]
        );
        assert_eq!(names("build"), [file("kept.log")]);
        assert_eq!(names("vendor"), [("lib".to_owned(), EntryKind::Repository)]);
        assert_eq!(
            list_dir(&dir, "src", &tracked).unwrap()[2].path,
            "src/lib.rs"
        );

        let LocalFile::Text(text) = read(&dir, "src/lib.rs").unwrap() else {
            panic!("expected text");
        };
        assert_eq!(text.line(0), "fn one() {}");
        assert_eq!(text.language.map(|l| l.name), Some("Rust"));
        assert!(matches!(read(&dir, "logo.png").unwrap(), LocalFile::Binary));
        assert!(matches!(read(&dir, "src").unwrap(), LocalFile::Directory));
        assert!(read(&dir, "gone.txt").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
