//! The files in a project's checkout as they are on disk, for browsing.
//! Shells out to `git` and reads files (blocking), so run on the
//! background executor.
//!
//! Git lists the files rather than a directory walk, so what's ignored
//! (`target/`, `node_modules/`) stays out: tracked files that are still
//! there, plus untracked ones that aren't ignored.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::Path;

use anyhow::{Context as _, Result, bail};

use super::git::git;
use crate::diff::TextSnapshot;
use crate::diff::git::{BINARY_SNIFF_BYTES, MAX_BLOB_BYTES};

/// A checkout's files as a tree, each directory's entries sorted with
/// directories first.
pub struct FileTree {
    /// The root is node 0.
    nodes: Vec<TreeNode>,
    pub file_count: usize,
    /// The checked-out branch, or the commit when detached.
    pub head: Option<String>,
}

pub struct TreeNode {
    pub name: String,
    /// Relative to the checkout's root, `/`-separated.
    pub path: String,
    /// Indices of the entries in a directory; `None` for files.
    pub children: Option<Vec<usize>>,
}

impl TreeNode {
    pub fn is_dir(&self) -> bool {
        self.children.is_some()
    }
}

impl FileTree {
    /// Builds the tree from `/`-separated paths of files.
    pub fn from_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Self {
        let mut nodes = vec![TreeNode {
            name: String::new(),
            path: String::new(),
            children: Some(Vec::new()),
        }];
        let mut dirs: HashMap<String, usize> = HashMap::new();
        let mut file_count = 0;
        for path in paths {
            let mut parent = 0;
            let mut parts = path.split('/').filter(|part| !part.is_empty()).peekable();
            while let Some(name) = parts.next() {
                let end = name.as_ptr() as usize - path.as_ptr() as usize + name.len();
                let is_file = parts.peek().is_none();
                let existing = (!is_file)
                    .then(|| dirs.get(&path[..end]).copied())
                    .flatten();
                let ix = existing.unwrap_or_else(|| {
                    let ix = nodes.len();
                    nodes.push(TreeNode {
                        name: name.to_owned(),
                        path: path[..end].to_owned(),
                        children: (!is_file).then(Vec::new),
                    });
                    if let Some(children) = &mut nodes[parent].children {
                        children.push(ix);
                    }
                    if is_file {
                        file_count += 1;
                    } else {
                        dirs.insert(path[..end].to_owned(), ix);
                    }
                    ix
                });
                parent = ix;
            }
        }

        for ix in 0..nodes.len() {
            let Some(mut children) = nodes[ix].children.take() else {
                continue;
            };
            children.sort_by(|&a, &b| {
                let (a, b) = (&nodes[a], &nodes[b]);
                b.is_dir()
                    .cmp(&a.is_dir())
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                    .then_with(|| a.name.cmp(&b.name))
            });
            nodes[ix].children = Some(children);
        }
        Self {
            nodes,
            file_count,
            head: None,
        }
    }

    pub fn root(&self) -> &[usize] {
        self.nodes[0].children.as_deref().unwrap_or_default()
    }

    pub fn node(&self, ix: usize) -> &TreeNode {
        &self.nodes[ix]
    }
}

/// Lists the checkout's files: what's tracked and still on disk, and
/// what's untracked but not ignored.
pub fn list(root: &Path) -> Result<FileTree> {
    let listed = git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let deleted = git(root, &["ls-files", "-z", "--deleted"])?;
    let deleted: HashSet<&str> = deleted.split('\0').collect();
    let mut seen = HashSet::new();
    let paths = listed
        .split('\0')
        .filter(|path| !path.is_empty() && !deleted.contains(path) && seen.insert(*path));
    let mut tree = FileTree::from_paths(paths);
    // Fails in a repository with no commits yet.
    tree.head = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|head| head.trim().to_owned())
        .and_then(|head| match head.as_str() {
            "HEAD" => git(root, &["rev-parse", "--short", "HEAD"])
                .ok()
                .map(|oid| oid.trim().to_owned()),
            _ => Some(head),
        });
    Ok(tree)
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

    /// Each node's path, indented by depth, depth-first in display order.
    fn outline(tree: &FileTree) -> Vec<String> {
        fn walk(tree: &FileTree, children: &[usize], depth: usize, out: &mut Vec<String>) {
            for &ix in children {
                let node = tree.node(ix);
                let suffix = if node.is_dir() { "/" } else { "" };
                out.push(format!("{}{}{suffix}", "  ".repeat(depth), node.name));
                walk(
                    tree,
                    node.children.as_deref().unwrap_or_default(),
                    depth + 1,
                    out,
                );
            }
        }
        let mut out = Vec::new();
        walk(tree, tree.root(), 0, &mut out);
        out
    }

    #[test]
    fn builds_sorted_tree() {
        let tree = FileTree::from_paths([
            "src/main.rs",
            "README.md",
            "src/ui/view.rs",
            "Cargo.toml",
            "src/app.rs",
            "assets/b.svg",
            "assets/A.svg",
        ]);
        assert_eq!(
            outline(&tree),
            [
                "assets/",
                "  A.svg",
                "  b.svg",
                "src/",
                "  ui/",
                "    view.rs",
                "  app.rs",
                "  main.rs",
                "Cargo.toml",
                "README.md",
            ]
        );
        assert_eq!(tree.file_count, 7);
        let ui = tree.root()[1];
        let ui = tree.node(tree.node(ui).children.as_ref().unwrap()[0]);
        assert_eq!(ui.path, "src/ui");
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
        write(".gitignore", b"target/\n");
        write("src/lib.rs", b"fn one() {}\n");
        write("gone.txt", b"bye\n");
        git(&dir, &["add", "-A"]).unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        write("notes.md", b"# Untracked\n");
        write("target/debug/out", b"ignored");
        write("logo.png", b"\x89PNG\0\0\0");

        let tree = list(&dir).unwrap();
        assert_eq!(
            outline(&tree),
            ["src/", "  lib.rs", ".gitignore", "logo.png", "notes.md"]
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
