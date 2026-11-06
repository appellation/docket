//! Files marked as viewed, as on GitHub's Files tab: each by its diff
//! (`DiffFiles::viewed_scope`, a pull request or a branch) and path, with
//! the [`ViewKey`] of what it changed when it was marked. A file whose key
//! has moved on since is no longer viewed, but changed since it was.
//!
//! They're kept in the platform's data directory
//! (`~/Library/Application Support/dev.docket/viewed.json` on macOS), and
//! a diff's are dropped once none of them has been marked in `MAX_AGE`.
//! Blocking I/O; run on the background executor. Tests keep nothing on
//! disk.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{FileContent, FileDiff, LineKind, lines_in};
use crate::config::APP_DIR_NAME;

/// How long a diff's marks are kept after the last one.
const MAX_AGE: Duration = Duration::days(180);

/// What a file's diff shows: its paths and status, and the lines of its
/// excerpts (or, for a file whose text isn't shown, both sides' blobs).
/// Lines outside the excerpts don't count, so a rebase that changes only
/// other parts of the file leaves it viewed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ViewKey(String);

impl ViewKey {
    /// `file`'s key; `old_oid` and `new_oid` are its sides' blobs.
    pub fn of(file: &FileDiff, old_oid: &str, new_oid: &str) -> Self {
        let mut hash = Sha256::new();
        let mut part = |bytes: &[u8]| {
            hash.update(bytes);
            hash.update([0]);
        };
        part(file.old_path.as_deref().unwrap_or_default().as_bytes());
        part(file.new_path.as_deref().unwrap_or_default().as_bytes());
        part(format!("{:?}", file.status).as_bytes());
        match &file.content {
            FileContent::Text(text) => {
                for excerpt in &text.excerpts {
                    part(b"@@");
                    for line in lines_in(&text.ops, excerpt) {
                        let (marker, side, number) = match line.kind {
                            LineKind::Context => (b' ', text.new.as_ref(), line.new_line),
                            LineKind::Added => (b'+', text.new.as_ref(), line.new_line),
                            LineKind::Removed => (b'-', text.old.as_ref(), line.old_line),
                        };
                        let content = side.zip(number).map_or("", |(side, n)| side.line(n));
                        part(&[&[marker], content.as_bytes()].concat());
                    }
                }
            }
            _ => {
                part(old_oid.as_bytes());
                part(new_oid.as_bytes());
            }
        }
        // Half the digest is plenty to tell one version of a file from
        // another.
        let key = hash.finalize()[..16]
            .iter()
            .fold(String::with_capacity(32), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        Self(key)
    }
}

/// Whether a file is marked as viewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Viewed {
    No,
    Yes,
    /// It was, but its diff has changed since.
    Changed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ViewedStore {
    /// By `DiffFiles::viewed_scope`.
    diffs: HashMap<String, ViewedDiff>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ViewedDiff {
    /// When a file was last marked or unmarked.
    touched: DateTime<Utc>,
    /// The key each viewed file had when it was marked, by path.
    files: HashMap<String, ViewKey>,
}

impl ViewedStore {
    pub fn viewed(&self, scope: &str, file: &FileDiff) -> Viewed {
        match self
            .diffs
            .get(scope)
            .and_then(|diff| diff.files.get(file.path()))
        {
            None => Viewed::No,
            Some(key) if *key == file.view_key => Viewed::Yes,
            Some(_) => Viewed::Changed,
        }
    }

    /// The key the file at `path` had when it was marked as viewed.
    pub fn key(&self, scope: &str, path: &str) -> Option<&ViewKey> {
        self.diffs.get(scope)?.files.get(path)
    }

    /// Marks the file at `path` as viewed while its key is `key`, or with
    /// `None`, unmarks it.
    pub fn set(&mut self, scope: &str, path: &str, key: Option<ViewKey>, now: DateTime<Utc>) {
        let diff = self
            .diffs
            .entry(scope.to_owned())
            .or_insert_with(|| ViewedDiff {
                touched: now,
                files: HashMap::new(),
            });
        diff.touched = now;
        match key {
            Some(key) => diff.files.insert(path.to_owned(), key),
            None => diff.files.remove(path),
        };
        if diff.files.is_empty() {
            self.diffs.remove(scope);
        }
    }

    /// Adds `other`'s diffs that this has no marks for: what was saved,
    /// under what was marked before it loaded.
    pub fn merge(&mut self, other: ViewedStore) {
        for (scope, diff) in other.diffs {
            self.diffs.entry(scope).or_insert(diff);
        }
    }

    fn path() -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        Some(dirs::data_dir()?.join(APP_DIR_NAME).join("viewed.json"))
    }

    /// What was saved. A file that can't be read is as good as none.
    pub fn load() -> Self {
        Self::path()
            .and_then(|path| fs::read(path).ok())
            .and_then(|json| serde_json::from_slice(&json).ok())
            .unwrap_or_default()
    }

    /// Drops diffs not marked in `MAX_AGE`, then writes atomically (temp
    /// file + rename) so a crash can't truncate it.
    pub fn save(mut self, now: DateTime<Utc>) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        self.diffs.retain(|_, diff| now - diff.touched < MAX_AGE);
        let dir = path.parent().expect("store path has a parent");
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{FileStatus, TextContent, TextSnapshot};

    fn file(path: &str, old: &str, new: &str) -> FileDiff {
        let snapshot = |s: &str| TextSnapshot::new(s.as_bytes().to_vec(), path);
        let mut file = FileDiff {
            old_path: Some(path.to_owned()),
            new_path: Some(path.to_owned()),
            status: FileStatus::Modified,
            additions: 0,
            deletions: 0,
            content: FileContent::Text(TextContent::new(Some(snapshot(old)), Some(snapshot(new)))),
            view_key: ViewKey::default(),
        };
        file.view_key = ViewKey::of(&file, "", "");
        file
    }

    fn numbered(lines: u32) -> String {
        (1..=lines).map(|n| format!("{n}\n")).collect()
    }

    #[test]
    fn keys_follow_what_the_diff_shows() {
        let old = numbered(40);
        let new = old.replace("10\n", "ten\n");
        let key = file("a.rs", &old, &new).view_key;
        assert_eq!(file("a.rs", &old, &new).view_key, key);
        // Another change to the file changes it.
        let changed = new.replace("11\n", "eleven\n");
        assert_ne!(file("a.rs", &old, &changed).view_key, key);
        // So does the same change to another file.
        assert_ne!(file("b.rs", &old, &new).view_key, key);
        // But not lines far from the change, on both sides, as after a
        // rebase onto a base that changed them.
        let rebased = |s: &str| s.replace("30\n", "thirty\n");
        assert_eq!(file("a.rs", &rebased(&old), &rebased(&new)).view_key, key);
    }

    #[test]
    fn viewed_until_the_file_changes() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let old = numbered(20);
        let a = file("a.rs", &old, &old.replace("5\n", "five\n"));
        let mut store = ViewedStore::default();
        assert_eq!(store.viewed("acme/widgets#1", &a), Viewed::No);

        store.set("acme/widgets#1", "a.rs", Some(a.view_key.clone()), now);
        assert_eq!(store.viewed("acme/widgets#1", &a), Viewed::Yes);
        // Only in that diff.
        assert_eq!(store.viewed("acme/widgets#2", &a), Viewed::No);

        let changed = file("a.rs", &old, &old.replace("5\n", "FIVE\n"));
        assert_eq!(store.viewed("acme/widgets#1", &changed), Viewed::Changed);

        store.set("acme/widgets#1", "a.rs", None, now);
        assert_eq!(store.viewed("acme/widgets#1", &a), Viewed::No);
        assert!(store.diffs.is_empty());
    }

    #[test]
    fn merging_keeps_what_was_marked_first() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let old = numbered(20);
        let a = file("a.rs", &old, &old.replace("5\n", "five\n"));
        let b = file("b.rs", &old, &old.replace("6\n", "six\n"));
        let mut saved = ViewedStore::default();
        saved.set("acme/widgets#1", "a.rs", Some(a.view_key.clone()), now);
        saved.set("acme/widgets#2", "b.rs", Some(b.view_key.clone()), now);
        let mut marked = ViewedStore::default();
        marked.set("acme/widgets#1", "b.rs", Some(b.view_key.clone()), now);
        marked.merge(saved);
        assert_eq!(marked.viewed("acme/widgets#1", &a), Viewed::No);
        assert_eq!(marked.viewed("acme/widgets#1", &b), Viewed::Yes);
        assert_eq!(marked.viewed("acme/widgets#2", &b), Viewed::Yes);
    }
}
