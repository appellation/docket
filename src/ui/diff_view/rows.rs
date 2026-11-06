//! Flattens a diff into the rows the unified layout renders.
//!
//! Every row is the same height, so the list can be virtualized. A `Line`
//! row keeps both sides' line numbers, which is what maps a row back to a
//! file position (for opening in an editor or LSP requests), and what a
//! split layout would pair up.
//!
//! Grouped by change (`build_grouped_rows`), each of the analysis's groups
//! heads its hunks, file by file: its title, then its summary and impact
//! wrapped into rows of their own, since rows can't grow to fit them. The
//! changed-files tree beside the diff lists the titles (`file_list.rs`).
//!
//! Comments left for a review (`Notes`) follow the line they end on: a
//! header, then their text wrapped into rows. So does the box a comment is
//! being written in, taking `COMPOSER_ROWS` rows that the view draws it
//! over (`review.rs`).

use std::collections::{HashMap, HashSet};

use crate::ai::{ChangeGroup, DiffAnalysis, HunkRef};
use crate::diff::{DiffLine, FileContent, LineKind, PullRequestDiff, TextContent, lines_in};
use crate::github::{DiffLineRef, DiffSide};
use crate::ui::code::display_width;

/// Where a group's summary and impact wrap, in characters. They're in the
/// interface font, narrower than the code's, so this is roughly as wide as
/// a hundred columns of code.
const WRAP_COLUMNS: usize = 110;

/// What starts a group's impact.
pub const IMPACT_LABEL: &str = "Impact: ";

/// Where a comment's text wraps, in characters.
const COMMENT_WRAP_COLUMNS: usize = 96;

/// The rows the box a comment is written in takes.
pub const COMPOSER_ROWS: usize = 10;

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
    /// A group of changes' title, above its hunks.
    GroupHeader {
        group: usize,
    },
    /// A line of a group's summary or impact: `DiffRows::group_text`.
    GroupText {
        group: usize,
        line: usize,
    },
    /// A comment left for the review: its header (`line` 0), then its
    /// text, a line at a time (`DiffRows::comments`).
    Comment {
        file: usize,
        comment: usize,
        line: usize,
    },
    /// One of the rows the box a comment is being written in covers.
    Composer {
        file: usize,
        line: usize,
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
    /// Grouped by change, the row of each group's header.
    pub group_headers: Vec<usize>,
    /// Grouped by change, the header of each file under each group: by
    /// group, then file.
    pub group_file_headers: HashMap<(usize, usize), usize>,
    /// Each group's summary and impact, wrapped.
    pub group_text: Vec<Vec<GroupLine>>,
    /// The comments shown, in order.
    pub comments: Vec<CommentRows>,
}

/// A comment as shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentRows {
    pub id: u64,
    /// The lines it's on, like `Line R12`.
    pub label: String,
    /// Its text, wrapped.
    pub lines: Vec<String>,
}

/// What to show after lines of the diff: comments, and the box a comment
/// is being written in. By file, then line.
#[derive(Default)]
pub struct Notes {
    pub comments: HashMap<(usize, DiffLineRef), Vec<NoteComment>>,
    pub composer: Option<(usize, DiffLineRef)>,
}

impl Notes {
    /// A comment with ID `id`, its label, and its text, after `line` of
    /// `file`.
    pub fn add_comment(
        &mut self,
        file: usize,
        line: DiffLineRef,
        id: u64,
        label: String,
        body: &str,
    ) {
        self.comments
            .entry((file, line))
            .or_default()
            .push(NoteComment {
                id,
                label,
                body: body.to_owned(),
            });
    }
}

/// A comment to show, by `Notes`.
pub struct NoteComment {
    id: u64,
    label: String,
    body: String,
}

/// The line a row shows, as GitHub numbers it: a removed line on the
/// base's side, the rest on the head's.
pub fn line_ref(line: DiffLine) -> Option<DiffLineRef> {
    match line.kind {
        LineKind::Removed => Some(DiffLineRef {
            side: DiffSide::Left,
            line: line.old_line? + 1,
        }),
        LineKind::Context | LineKind::Added => Some(DiffLineRef {
            side: DiffSide::Right,
            line: line.new_line? + 1,
        }),
    }
}

/// A comment's text wrapped into rows, keeping its blank lines.
fn wrap_comment(body: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in body.trim_end().lines() {
        let wrapped = wrap(paragraph, COMMENT_WRAP_COLUMNS);
        if wrapped.is_empty() {
            lines.push(String::new());
        } else {
            lines.extend(wrapped);
        }
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupLine {
    pub text: String,
    pub impact: bool,
    /// The impact's first line, which its label starts.
    pub labeled: bool,
}

impl DiffRows {
    /// The group `row` is in, grouped by change.
    pub fn group_at(&self, row: usize) -> Option<usize> {
        self.group_headers
            .partition_point(|&header| header <= row)
            .checked_sub(1)
    }
}

/// Builds rows, tracking which is widest.
struct Builder<'a> {
    diff: &'a PullRequestDiff,
    collapsed: &'a HashSet<usize>,
    notes: &'a Notes,
    out: DiffRows,
    widest: usize,
}

impl<'a> Builder<'a> {
    fn new(diff: &'a PullRequestDiff, collapsed: &'a HashSet<usize>, notes: &'a Notes) -> Self {
        let out = DiffRows {
            file_headers: vec![0; diff.files.len()],
            ..Default::default()
        };
        Self {
            diff,
            collapsed,
            notes,
            out,
            widest: 0,
        }
    }

    /// The comments, and the box being written in, after `line`.
    fn notes_after(&mut self, file: usize, line: DiffLine) {
        let Some(at) = line_ref(line) else {
            return;
        };
        for note in self.notes.comments.get(&(file, at)).into_iter().flatten() {
            let comment = self.out.comments.len();
            let lines = wrap_comment(&note.body);
            self.out
                .rows
                .extend((0..=lines.len()).map(|line| DisplayRow::Comment {
                    file,
                    comment,
                    line,
                }));
            self.out.comments.push(CommentRows {
                id: note.id,
                label: note.label.clone(),
                lines,
            });
        }
        if self.notes.composer == Some((file, at)) {
            self.out
                .rows
                .extend((0..COMPOSER_ROWS).map(|line| DisplayRow::Composer { file, line }));
        }
    }

    /// A file's header, the first time it's shown the one it scrolls to.
    /// Whether the file is expanded.
    fn header(&mut self, file: usize, first: bool) -> bool {
        if first {
            self.out.file_headers[file] = self.out.rows.len();
        }
        if let Some(group) = self.out.group_headers.len().checked_sub(1) {
            self.out
                .group_file_headers
                .insert((group, file), self.out.rows.len());
        }
        self.out.rows.push(DisplayRow::FileHeader { file });
        !self.collapsed.contains(&file)
    }

    /// The file's text, or the notice in its place if it has none to show.
    fn text(&mut self, file: usize) -> Option<&'a TextContent> {
        let notice = match &self.diff.files[file].content {
            FileContent::Text(text) if !text.excerpts.is_empty() => return Some(text),
            FileContent::Text(_) => Notice::NoChanges,
            FileContent::Binary => Notice::Binary,
            FileContent::Submodule => Notice::Submodule,
            FileContent::TooLarge { bytes } => Notice::TooLarge { bytes: *bytes },
        };
        self.out.rows.push(DisplayRow::Notice { file, notice });
        None
    }

    fn excerpt(&mut self, file: usize, text: &TextContent, ix: usize) {
        let Some(excerpt) = text.excerpts.get(ix) else {
            return;
        };
        self.out
            .rows
            .push(DisplayRow::HunkHeader { file, excerpt: ix });
        for line in lines_in(&text.ops, excerpt) {
            let snapshot = match line.new_line {
                Some(new) => text.new.as_ref().map(|s| s.line(new)),
                None => text.old.as_ref().zip(line.old_line).map(|(s, l)| s.line(l)),
            };
            let width = snapshot.map_or(0, display_width);
            if width > self.widest {
                self.widest = width;
                self.out.widest = Some(self.out.rows.len());
            }
            self.out.rows.push(DisplayRow::Line { file, line });
            self.notes_after(file, line);
        }
    }
}

pub fn build_rows(diff: &PullRequestDiff, collapsed: &HashSet<usize>, notes: &Notes) -> DiffRows {
    let mut rows = Builder::new(diff, collapsed, notes);
    for file in 0..diff.files.len() {
        if file > 0 {
            rows.out.rows.push(DisplayRow::Spacer);
        }
        if !rows.header(file, true) {
            continue;
        }
        if let Some(text) = rows.text(file) {
            for ix in 0..text.excerpts.len() {
                rows.excerpt(file, text, ix);
            }
        }
    }
    rows.out
}

/// The diff grouped by change: each group, then its hunks, file by file.
/// A file with hunks in several groups has a header in each.
pub fn build_grouped_rows(
    diff: &PullRequestDiff,
    analysis: &DiffAnalysis,
    collapsed: &HashSet<usize>,
    notes: &Notes,
) -> DiffRows {
    let mut rows = Builder::new(diff, collapsed, notes);
    let mut shown = HashSet::new();
    for (group, change) in analysis.groups.iter().enumerate() {
        if group > 0 {
            rows.out.rows.push(DisplayRow::Spacer);
        }
        rows.out.group_headers.push(rows.out.rows.len());
        rows.out.rows.push(DisplayRow::GroupHeader { group });
        let text = group_text(change);
        rows.out
            .rows
            .extend((0..text.len()).map(|line| DisplayRow::GroupText { group, line }));
        rows.out.group_text.push(text);

        let hunks: Vec<&HunkRef> = change
            .hunks
            .iter()
            .filter(|hunk| hunk.file < diff.files.len())
            .collect();
        for file_hunks in hunks.chunk_by(|a, b| a.file == b.file) {
            let file = file_hunks[0].file;
            rows.out.rows.push(DisplayRow::Spacer);
            if !rows.header(file, shown.insert(file)) {
                continue;
            }
            let Some(text) = rows.text(file) else {
                continue;
            };
            for hunk in file_hunks {
                match hunk.excerpt {
                    Some(ix) => rows.excerpt(file, text, ix),
                    // A whole file, though it has text: show all of it.
                    None => (0..text.excerpts.len()).for_each(|ix| rows.excerpt(file, text, ix)),
                }
            }
        }
    }
    rows.out
}

/// A group's summary, then its impact after a label, wrapped.
fn group_text(group: &ChangeGroup) -> Vec<GroupLine> {
    let mut lines: Vec<GroupLine> = wrap(&group.summary, WRAP_COLUMNS)
        .into_iter()
        .map(|text| GroupLine {
            text,
            impact: false,
            labeled: false,
        })
        .collect();
    if !group.impact.is_empty() {
        let impact = format!("{IMPACT_LABEL}{}", group.impact);
        lines.extend(
            wrap(&impact, WRAP_COLUMNS)
                .into_iter()
                .enumerate()
                .map(|(ix, text)| GroupLine {
                    text,
                    impact: true,
                    labeled: ix == 0,
                }),
        );
    }
    lines
}

/// Breaks `text` into lines of at most `width` characters, between words
/// where it can.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines().filter(|p| !p.trim().is_empty()) {
        let mut line = String::new();
        let mut len = 0;
        for word in paragraph.split_whitespace() {
            let mut word = word;
            let mut word_len = word.chars().count();
            if len > 0 && len + 1 + word_len > width {
                lines.push(std::mem::take(&mut line));
                len = 0;
            }
            // A word longer than a line breaks where it must.
            while word_len > width {
                let split = word
                    .char_indices()
                    .nth(width)
                    .map_or(word.len(), |(i, _)| i);
                lines.push(word[..split].to_owned());
                word = &word[split..];
                word_len -= width;
            }
            if len > 0 {
                line.push(' ');
                len += 1;
            }
            line.push_str(word);
            len += word_len;
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
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
            view_key: crate::diff::ViewKey::default(),
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
                DisplayRow::GroupHeader { group } => format!("group {group}"),
                DisplayRow::GroupText { .. } => "text".into(),
                DisplayRow::Comment { line: 0, .. } => "comment".into(),
                DisplayRow::Comment { .. } => "comment text".into(),
                DisplayRow::Composer { .. } => "composer".into(),
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
                view_key: crate::diff::ViewKey::default(),
            },
            text_file("same.rs", "x\n", "x\n"),
        ]);
        let rows = build_rows(&diff, &HashSet::new(), &Notes::default());
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
        let rows = build_rows(&diff, &HashSet::from([0]), &Notes::default());
        assert_eq!(
            kinds(&rows),
            ["file 0", "spacer", "file 1", "hunk 0", "-", "+"]
        );
    }

    #[test]
    fn groups_head_their_hunks() {
        let numbered: String = (1..=30).map(|n| format!("{n}\n")).collect();
        let changed = numbered
            .replace("2\n", "two\n")
            .replace("25\n", "twenty-five\n");
        let diff = diff(vec![
            text_file("a.rs", &numbered, &changed),
            text_file("b.rs", "one\n", "two\n"),
        ]);
        let hunk = |file, excerpt| HunkRef {
            file,
            excerpt: Some(excerpt),
        };
        let group = |title: &str, impact: &str, hunks| ChangeGroup {
            title: title.into(),
            summary: "Summary.".into(),
            impact: impact.into(),
            hunks,
        };
        let analysis = DiffAnalysis {
            groups: vec![
                group("Rename", "Callers change.", vec![hunk(0, 0), hunk(1, 0)]),
                group("Tidy", "", vec![hunk(0, 1)]),
            ],
        };
        let rows = build_grouped_rows(&diff, &analysis, &HashSet::new(), &Notes::default());
        let kinds = kinds(&rows);
        assert_eq!(kinds[..5], ["group 0", "text", "text", "spacer", "file 0"]);
        let line = |text: &str, impact, labeled| GroupLine {
            text: text.into(),
            impact,
            labeled,
        };
        assert_eq!(
            rows.group_text[0],
            [
                line("Summary.", false, false),
                line("Impact: Callers change.", true, true)
            ]
        );
        assert_eq!(rows.group_text[1].len(), 1, "no impact, no line for it");
        let position = |kind: &str, nth: usize| {
            kinds
                .iter()
                .enumerate()
                .filter(|(_, k)| *k == kind)
                .nth(nth)
                .unwrap()
                .0
        };
        assert_eq!(rows.group_headers, [0, position("group 1", 0)]);
        // a.rs shows under both groups, and scrolls to its first.
        let headers: Vec<_> = kinds.iter().filter(|k| k.starts_with("file")).collect();
        assert_eq!(headers, ["file 0", "file 1", "file 0"]);
        assert_eq!(rows.file_headers, [4, position("file 1", 0)]);
        assert_eq!(rows.group_file_headers[&(1, 0)], position("file 0", 1));
        assert_eq!(rows.group_at(0), Some(0));
        assert_eq!(rows.group_at(rows.rows.len() - 1), Some(1));
    }

    #[test]
    fn comments_follow_their_lines() {
        let diff = diff(vec![text_file("a.rs", "one\ntwo\n", "one\nthree\n")]);
        let right = |line| DiffLineRef {
            side: DiffSide::Right,
            line,
        };
        let mut notes = Notes::default();
        notes.add_comment(
            0,
            right(2),
            7,
            "Line R2".into(),
            "Why three?\n\nIt was two.",
        );
        notes.add_comment(
            0,
            DiffLineRef {
                side: DiffSide::Left,
                line: 2,
            },
            8,
            "Line L2".into(),
            "Gone.",
        );
        notes.composer = Some((0, right(1)));
        let rows = build_rows(&diff, &HashSet::new(), &notes);
        let mut expected = vec!["file 0", "hunk 0", " "];
        expected.extend(["composer"; COMPOSER_ROWS]);
        expected.extend(["-", "comment", "comment text", "+", "comment"]);
        expected.extend(["comment text"; 3]);
        assert_eq!(kinds(&rows), expected);
        assert_eq!(rows.comments[0].id, 8);
        assert_eq!(rows.comments[1].lines, ["Why three?", "", "It was two."]);
    }

    #[test]
    fn wraps_between_words() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("abcdefghij k", 4), ["abcd", "efgh", "ij k"]);
        assert!(wrap("", 9).is_empty());
    }
}
