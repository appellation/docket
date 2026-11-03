//! Flattens the listed directories into the rows the tree list renders:
//! each directory's entries in order, with an expanded directory's entries
//! right under it once it's been listed.

use std::collections::{HashMap, HashSet};

use crate::projects::files::{DirEntry, EntryKind};
use crate::state::DirLoad;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub entry: DirEntry,
    /// 0 for the checkout's top-level entries.
    pub depth: usize,
}

/// The visible rows, given each listed directory and the paths of the
/// expanded ones.
pub fn visible_rows(dirs: &HashMap<String, DirLoad>, expanded: &HashSet<String>) -> Vec<TreeRow> {
    let listing = |path: &str| match dirs.get(path) {
        Some(DirLoad::Ready(entries)) => entries.as_slice(),
        _ => &[],
    };
    let mut rows = Vec::new();
    // Reversed, so popping visits entries in order.
    let mut stack: Vec<(&DirEntry, usize)> = listing("").iter().rev().map(|e| (e, 0)).collect();
    while let Some((entry, depth)) = stack.pop() {
        if entry.kind == EntryKind::Dir && expanded.contains(&entry.path) {
            stack.extend(listing(&entry.path).iter().rev().map(|e| (e, depth + 1)));
        }
        rows.push(TreeRow {
            entry: entry.clone(),
            depth,
        });
    }
    rows
}

/// The row of the directory `ix` is in, if it isn't top-level.
pub fn parent_row(rows: &[TreeRow], ix: usize) -> Option<usize> {
    let depth = rows.get(ix)?.depth;
    rows[..ix].iter().rposition(|row| row.depth < depth)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn listing(entries: &[(&str, EntryKind)]) -> DirLoad {
        DirLoad::Ready(Arc::new(
            entries
                .iter()
                .map(|&(path, kind)| DirEntry {
                    name: path.rsplit('/').next().unwrap().to_owned(),
                    path: path.to_owned(),
                    kind,
                })
                .collect(),
        ))
    }

    fn names(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.entry.name))
            .collect()
    }

    #[test]
    fn shows_expanded_directories_entries() {
        use EntryKind::{Dir, File};
        let mut dirs = HashMap::from([
            (
                String::new(),
                listing(&[("docs", Dir), ("src", Dir), ("README.md", File)]),
            ),
            (
                "src".to_owned(),
                listing(&[("src/ui", Dir), ("src/main.rs", File)]),
            ),
            ("src/ui".to_owned(), listing(&[("src/ui/view.rs", File)])),
        ]);
        let collapsed = visible_rows(&dirs, &HashSet::new());
        assert_eq!(names(&collapsed), ["docs", "src", "README.md"]);

        // An expanded directory inside a collapsed one stays hidden.
        let mut expanded = HashSet::from(["src/ui".to_owned()]);
        assert_eq!(visible_rows(&dirs, &expanded), collapsed);

        expanded.insert("src".to_owned());
        let rows = visible_rows(&dirs, &expanded);
        assert_eq!(
            names(&rows),
            [
                "docs",
                "src",
                "  ui",
                "    view.rs",
                "  main.rs",
                "README.md"
            ]
        );
        assert_eq!(parent_row(&rows, 3), Some(2));
        assert_eq!(parent_row(&rows, 4), Some(1));
        assert_eq!(parent_row(&rows, 5), None);
        assert_eq!(parent_row(&rows, 1), None);

        // Until it's listed, an expanded directory shows nothing under it.
        dirs.insert("docs".to_owned(), DirLoad::Loading);
        expanded.insert("docs".to_owned());
        assert_eq!(names(&visible_rows(&dirs, &expanded))[..2], ["docs", "src"]);
    }
}
