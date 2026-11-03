//! Flattens a diff into the rows the unified layout renders.
//!
//! Every row is the same height, so the list can be virtualized. A `Line`
//! row keeps both sides' line numbers, which is what maps a row back to a
//! file position (for opening in an editor or LSP requests), and what a
//! split layout would pair up.

use std::collections::HashSet;

use crate::diff::{DiffLine, FileContent, PullRequestDiff, lines_in};
use crate::ui::code::display_width;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRow {
    /// Blank space before each file.
    Spacer,
    FileHeader {
        file: usize,
    },
    HunkHeader {
        file: usize,
        excerpt: usize,
    },
    Line {
        file: usize,
        line: DiffLine,
    },
    Notice {
        file: usize,
        notice: Notice,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Binary,
    Submodule,
    TooLarge {
        bytes: u64,
    },
    /// Renamed or mode-changed without content changes.
    NoChanges,
}

#[derive(Default)]
pub struct DiffRows {
    pub rows: Vec<DisplayRow>,
    /// Row index of each file's header.
    pub file_headers: Vec<usize>,
    /// The row with the widest text, which sizes the list horizontally.
    pub widest: Option<usize>,
}

pub fn build_rows(diff: &PullRequestDiff, collapsed: &HashSet<usize>) -> DiffRows {
    let mut out = DiffRows::default();
    let mut widest = 0;
    for (file, file_diff) in diff.files.iter().enumerate() {
        if file > 0 {
            out.rows.push(DisplayRow::Spacer);
        }
        out.file_headers.push(out.rows.len());
        out.rows.push(DisplayRow::FileHeader { file });
        if collapsed.contains(&file) {
            continue;
        }

        let notice = |notice| DisplayRow::Notice { file, notice };
        let text = match &file_diff.content {
            FileContent::Text(text) => text,
            FileContent::Binary => {
                out.rows.push(notice(Notice::Binary));
                continue;
            }
            FileContent::Submodule => {
                out.rows.push(notice(Notice::Submodule));
                continue;
            }
            FileContent::TooLarge { bytes } => {
                out.rows.push(notice(Notice::TooLarge { bytes: *bytes }));
                continue;
            }
        };
        if text.excerpts.is_empty() {
            out.rows.push(notice(Notice::NoChanges));
            continue;
        }
        for (ix, excerpt) in text.excerpts.iter().enumerate() {
            out.rows.push(DisplayRow::HunkHeader { file, excerpt: ix });
            for line in lines_in(&text.ops, excerpt) {
                let snapshot = match line.new_line {
                    Some(new) => text.new.as_ref().map(|s| s.line(new)),
                    None => text.old.as_ref().zip(line.old_line).map(|(s, l)| s.line(l)),
                };
                let width = snapshot.map_or(0, display_width);
                if width > widest {
                    widest = width;
                    out.widest = Some(out.rows.len());
                }
                out.rows.push(DisplayRow::Line { file, line });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{FileDiff, FileStatus, LineKind, TextContent, TextSnapshot};

    fn text_file(path: &str, old: &str, new: &str) -> FileDiff {
        let snapshot = |s: &str| TextSnapshot::new(s.as_bytes().to_vec(), path);
        let content = TextContent::new(Some(snapshot(old)), Some(snapshot(new)));
        let (additions, deletions) = content.stats();
        FileDiff {
            old_path: Some(path.into()),
            new_path: Some(path.into()),
            status: FileStatus::Modified,
            additions,
            deletions,
            content: FileContent::Text(content),
        }
    }

    fn diff(files: Vec<FileDiff>) -> PullRequestDiff {
        PullRequestDiff {
            head_oid: String::new(),
            files,
        }
    }

    fn kinds(rows: &DiffRows) -> Vec<String> {
        rows.rows
            .iter()
            .map(|row| match row {
                DisplayRow::Spacer => "spacer".into(),
                DisplayRow::FileHeader { file } => format!("file {file}"),
                DisplayRow::HunkHeader { excerpt, .. } => format!("hunk {excerpt}"),
                DisplayRow::Line { line, .. } => match line.kind {
                    LineKind::Context => " ".into(),
                    LineKind::Added => "+".into(),
                    LineKind::Removed => "-".into(),
                },
                DisplayRow::Notice { notice, .. } => format!("{notice:?}"),
            })
            .collect()
    }

    #[test]
    fn builds_rows_for_files() {
        let diff = diff(vec![
            text_file("a.rs", "one\ntwo\n", "one\n\tlonger two\n"),
            FileDiff {
                old_path: None,
                new_path: Some("logo.png".into()),
                status: FileStatus::Added,
                additions: 0,
                deletions: 0,
                content: FileContent::Binary,
            },
            text_file("same.rs", "x\n", "x\n"),
        ]);
        let rows = build_rows(&diff, &HashSet::new());
        assert_eq!(
            kinds(&rows),
            [
                "file 0",
                "hunk 0",
                " ",
                "-",
                "+",
                "spacer",
                "file 1",
                "Binary",
                "spacer",
                "file 2",
                "NoChanges"
            ]
        );
        assert_eq!(rows.file_headers, [0, 6, 9]);
        // `\tlonger two` is the widest once the tab expands.
        assert_eq!(rows.widest, Some(4));
    }

    #[test]
    fn collapsed_files_show_only_their_header() {
        let diff = diff(vec![
            text_file("a.rs", "one\n", "two\n"),
            text_file("b.rs", "one\n", "two\n"),
        ]);
        let rows = build_rows(&diff, &HashSet::from([0]));
        assert_eq!(
            kinds(&rows),
            ["file 0", "spacer", "file 1", "hunk 0", "-", "+"]
        );
    }
}
