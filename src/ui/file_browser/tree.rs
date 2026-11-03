//! Flattens the file tree into the rows the tree list renders: each
//! directory's entries in order, with an expanded directory's entries
//! right under it.

use std::collections::HashSet;

use crate::projects::files::FileTree;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// Index into the tree's nodes.
    pub node: usize,
    /// 0 for the checkout's top-level entries.
    pub depth: usize,
}

/// The visible rows, given the paths of the expanded directories.
pub fn visible_rows(tree: &FileTree, expanded: &HashSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    // Reversed, so popping visits entries in order.
    let mut stack: Vec<TreeRow> = tree
        .root()
        .iter()
        .rev()
        .map(|&node| TreeRow { node, depth: 0 })
        .collect();
    while let Some(row) = stack.pop() {
        rows.push(row);
        let node = tree.node(row.node);
        if let Some(children) = &node.children
            && expanded.contains(&node.path)
        {
            stack.extend(children.iter().rev().map(|&node| TreeRow {
                node,
                depth: row.depth + 1,
            }));
        }
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
    use super::*;

    fn names(tree: &FileTree, rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let indent = "  ".repeat(row.depth);
                format!("{indent}{}", tree.node(row.node).name)
            })
            .collect()
    }

    #[test]
    fn shows_expanded_directories_entries() {
        let tree =
            FileTree::from_paths(["src/ui/view.rs", "src/main.rs", "docs/a.md", "README.md"]);
        let collapsed = visible_rows(&tree, &HashSet::new());
        assert_eq!(names(&tree, &collapsed), ["docs", "src", "README.md"]);

        // An expanded directory inside a collapsed one stays hidden.
        let mut expanded = HashSet::from(["src/ui".to_owned()]);
        assert_eq!(visible_rows(&tree, &expanded), collapsed);

        expanded.insert("src".to_owned());
        let rows = visible_rows(&tree, &expanded);
        assert_eq!(
            names(&tree, &rows),
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
    }
}
