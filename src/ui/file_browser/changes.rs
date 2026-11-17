//! The diff's changes to the open file, in a pull request's or branch's
//! Browse tab: the file as it is at the head, with the lines the diff adds
//! tinted, and the lines it removes tinted among them where they were, as
//! a unified diff of the whole file shows them.
//!
//! So a row isn't always a line. `FileRow` says what each row is, and
//! `FileBrowserView::row_of_line` where a line is; what works in lines
//! (find, symbols, definitions, the language server) maps through them.
//! Removed rows select and copy like the rest, but have no hover or
//! definitions: they aren't in the file the server has.
//!
//! The rows come from the diff's own alignment when the file reads as the
//! diff's head does, as it does in Docket's worktree. A branch browsed in
//! its checkout may have been edited since, so then the diff's old side is
//! aligned with the file as it reads, on the background executor, and the
//! file shows without changes until that's done. The header's button hides
//! the changes, and shows them again.

use std::sync::Arc;

use gpui::{Context, Pixels, Point, Task, px};

use super::FileBrowserView;
use crate::diff::{self, LineOp, OpKind, TextSnapshot};
use crate::ui::code::{ROW_HEIGHT, display_width};

/// What a row of the open file shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileRow {
    /// A line of the file (0-based), and whether the diff added it.
    Line { line: u32, added: bool },
    /// A line the diff removed, by its 0-based number on the old side.
    Removed { old_line: u32 },
}

/// The open file's rows with the diff's changes among them.
pub(super) struct FileChanges {
    pub rows: Vec<FileRow>,
    /// The row each line of the file is on.
    line_rows: Vec<usize>,
    /// The old side, which removed rows read from.
    pub old: Option<Arc<TextSnapshot>>,
    pub added: u32,
    pub removed: u32,
    /// The widest removed row, and its width in columns, to size the list.
    pub widest_removed: Option<(usize, usize)>,
}

impl FileChanges {
    /// The rows of `ops`, aligning `old` with the file. `None` if nothing
    /// changed.
    pub fn new(ops: &[LineOp], old: Option<Arc<TextSnapshot>>) -> Option<Self> {
        if ops.iter().all(|op| op.kind == OpKind::Equal) {
            return None;
        }
        let mut changes = Self {
            rows: Vec::new(),
            line_rows: Vec::new(),
            old,
            added: 0,
            removed: 0,
            widest_removed: None,
        };
        for op in ops {
            match op.kind {
                OpKind::Equal | OpKind::Insert => {
                    let added = op.kind == OpKind::Insert;
                    for line in op.new.clone() {
                        changes.line_rows.push(changes.rows.len());
                        changes.rows.push(FileRow::Line { line, added });
                    }
                    if added {
                        changes.added += op.new.len() as u32;
                    }
                }
                OpKind::Delete => {
                    for old_line in op.old.clone() {
                        let row = changes.rows.len();
                        changes.rows.push(FileRow::Removed { old_line });
                        let width = changes
                            .old
                            .as_ref()
                            .filter(|old| old_line < old.line_count())
                            .map_or(0, |old| display_width(old.line(old_line)));
                        if changes.widest_removed.is_none_or(|(_, w)| width > w) {
                            changes.widest_removed = Some((row, width));
                        }
                    }
                    changes.removed += op.old.len() as u32;
                }
            }
        }
        Some(changes)
    }

    /// The row `line` of the file is on.
    pub fn row_of(&self, line: usize) -> usize {
        self.line_rows
            .get(line)
            .copied()
            .unwrap_or_else(|| self.rows.len().saturating_sub(1))
    }

    /// The line at `row`, or the first after it if it's removed (the last
    /// line, past them all).
    pub fn line_at_or_after(&self, row: usize) -> usize {
        self.rows
            .iter()
            .skip(row)
            .find_map(|row| match row {
                FileRow::Line { line, .. } => Some(*line as usize),
                FileRow::Removed { .. } => None,
            })
            .unwrap_or(self.line_rows.len().saturating_sub(1))
    }
}

/// The diff's changes to the open file, and whether they're shown.
pub(super) struct ChangesState {
    /// Off once the header's button hides them.
    pub shown: bool,
    pub changes: Option<Arc<FileChanges>>,
    /// Aligning the diff's old side with the file, which shows without
    /// changes meanwhile.
    pub pending: bool,
    /// The file's text and the diff's sides `changes` are for.
    changes_for: Option<ChangesKey>,
    _task: Option<Task<()>>,
}

impl Default for ChangesState {
    fn default() -> Self {
        Self {
            shown: true,
            changes: None,
            pending: false,
            changes_for: None,
            _task: None,
        }
    }
}

type Snapshot = Option<Arc<TextSnapshot>>;

/// The file's text, and the old and new sides of the diff's file.
type ChangesKey = (Arc<TextSnapshot>, Option<(Snapshot, Snapshot)>);

fn same(a: &Snapshot, b: &Snapshot) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn same_key(a: &Option<ChangesKey>, b: &Option<ChangesKey>) -> bool {
    match (a, b) {
        (Some((a_text, a_diff)), Some((b_text, b_diff))) => {
            Arc::ptr_eq(a_text, b_text)
                && match (a_diff, b_diff) {
                    (Some((a_old, a_new)), Some((b_old, b_new))) => {
                        same(a_old, b_old) && same(a_new, b_new)
                    }
                    (None, None) => true,
                    _ => false,
                }
        }
        (None, None) => true,
        _ => false,
    }
}

impl FileBrowserView {
    /// The changes as shown: `None` while hidden, and for a file the diff
    /// doesn't change.
    pub(super) fn shown_changes(&self) -> Option<&Arc<FileChanges>> {
        self.changes.changes.as_ref().filter(|_| self.changes.shown)
    }

    /// What `row` shows, if it's in the file.
    pub(super) fn file_row(&self, row: usize) -> Option<FileRow> {
        match self.shown_changes() {
            Some(changes) => changes.rows.get(row).copied(),
            None => (row < self.line_count()).then_some(FileRow::Line {
                line: row as u32,
                added: false,
            }),
        }
    }

    /// The row a line of the file is on.
    pub(super) fn row_of_line(&self, line: usize) -> usize {
        self.shown_changes()
            .map_or(line, |changes| changes.row_of(line))
    }

    /// Works out the changes again when the file's text or the diff's
    /// changes to it do. Before anything maps lines to rows.
    pub(super) fn sync_changes(&mut self, cx: &mut Context<Self>) {
        let text = self.open_text(cx);
        let diff = text.as_ref().and_then(|_| {
            let diff = self.state.read(cx).browser_file_diff()?;
            Some((diff.old.clone(), diff.new.clone()))
        });
        let key = text.clone().map(|text| (text, diff.clone()));
        if same_key(&self.changes.changes_for, &key) {
            return;
        }
        self.changes.changes_for = key;
        self.changes._task = None;
        self.changes.pending = false;
        let (Some(text), Some((old, new))) = (text, diff) else {
            self.set_changes(None);
            return;
        };
        // The diff's own alignment, if it's of this text.
        if new.as_ref().is_some_and(|new| new.text() == text.text()) {
            let changes = self
                .state
                .read(cx)
                .browser_file_diff()
                .and_then(|diff| FileChanges::new(&diff.ops, old));
            self.set_changes(changes);
            return;
        }
        self.set_changes(None);
        self.changes.pending = true;
        self.changes._task = Some(cx.spawn(async move |this, cx| {
            let changes = cx
                .background_executor()
                .spawn(async move {
                    let empty = TextSnapshot::empty();
                    let ops = diff::align(old.as_deref().unwrap_or(&empty), &text);
                    FileChanges::new(&ops, old)
                })
                .await;
            this.update(cx, |this, cx| {
                this.changes.pending = false;
                this.set_changes(changes);
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_changes(&mut self, changes: Option<FileChanges>) {
        let had = self.shown_changes().is_some();
        self.changes.changes = changes.map(Arc::new);
        if had || self.shown_changes().is_some() {
            self.rows_changed();
        }
    }

    /// The rows are different: positions in them mean something else.
    pub(super) fn rows_changed(&mut self) {
        self.rows_version += 1;
        self.code.reset();
    }

    /// The header's button: hides or shows the changes, keeping the line
    /// at the top where it is.
    pub(super) fn toggle_changes(&mut self, cx: &mut Context<Self>) {
        let (top, _) = self.visible_rows();
        let line = self
            .shown_changes()
            .map_or(top, |changes| changes.line_at_or_after(top));
        self.changes.shown = !self.changes.shown;
        self.rows_changed();
        let row = self.row_of_line(line);
        let scroll = self.code.scroll.0.borrow();
        let offset = scroll.base_handle.offset();
        scroll.base_handle.set_offset(Point::<Pixels> {
            x: offset.x,
            y: -px(ROW_HEIGHT) * row as f32,
        });
        drop(scroll);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(text: &str) -> TextSnapshot {
        TextSnapshot::new(text.as_bytes().to_vec(), "a.txt")
    }

    #[test]
    fn puts_removed_lines_among_the_files() {
        let old = snapshot("one\ntwo\nthree\nfour\n");
        let new = snapshot("one\n2\nthree\nfour\nfive\n");
        let ops = diff::align(&old, &new);
        let changes = FileChanges::new(&ops, Some(Arc::new(old))).unwrap();
        let line = |line, added| FileRow::Line { line, added };
        assert_eq!(
            changes.rows,
            [
                line(0, false),
                FileRow::Removed { old_line: 1 },
                line(1, true),
                line(2, false),
                line(3, false),
                line(4, true),
            ]
        );
        assert_eq!((changes.added, changes.removed), (2, 1));
        assert_eq!(
            (0..5).map(|l| changes.row_of(l)).collect::<Vec<_>>(),
            [0, 2, 3, 4, 5]
        );
        assert_eq!(changes.line_at_or_after(1), 1, "past the removed row");
        assert_eq!(changes.widest_removed, Some((1, 3)));
    }

    #[test]
    fn nothing_changed_is_no_changes() {
        let old = snapshot("one\n");
        let ops = diff::align(&old, &snapshot("one\n"));
        assert!(FileChanges::new(&ops, Some(Arc::new(old))).is_none());
    }
}
