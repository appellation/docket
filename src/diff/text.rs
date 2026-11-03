//! One side of a file in a diff: its full text, indexed by line.

use std::ops::Range;

use crate::syntax::{Language, LanguageRegistry};

/// A file's full contents at one revision. Diffs keep whole files, not just
/// the changed hunks, so highlighting sees real context and features like
/// expanding context or LSP have real positions to work with.
pub struct TextSnapshot {
    text: String,
    /// Byte offset of each line's start. Lines are 0-based.
    line_starts: Vec<u32>,
    pub language: Option<&'static Language>,
}

impl TextSnapshot {
    /// Decodes `bytes` lossily, so a stray non-UTF-8 byte doesn't hide the
    /// whole file.
    pub fn new(bytes: Vec<u8>, path: &str) -> Self {
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
        };
        let line_starts = text
            .split_inclusive('\n')
            .scan(0, |offset, line| {
                let start = *offset;
                *offset += line.len() as u32;
                Some(start)
            })
            .collect();
        Self {
            text,
            line_starts,
            language: LanguageRegistry::for_path(path),
        }
    }

    pub fn empty() -> Self {
        Self {
            text: String::new(),
            line_starts: Vec::new(),
            language: None,
        }
    }

    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Byte range of `line`, without its line ending.
    pub fn line_range(&self, line: u32) -> Range<usize> {
        let start = self.line_starts[line as usize] as usize;
        let end = self
            .line_starts
            .get(line as usize + 1)
            .map_or(self.text.len(), |&next| next as usize);
        let content = self.text[start..end].trim_end_matches(['\n', '\r']);
        start..start + content.len()
    }

    pub fn line(&self, line: u32) -> &str {
        &self.text[self.line_range(line)]
    }

    /// Lines including their endings, so `foo` and `foo\n` at the end of a
    /// file compare as different, as in Git.
    pub(super) fn lines_with_endings(&self) -> Vec<&str> {
        self.text.split_inclusive('\n').collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_lines() {
        let text = TextSnapshot::new(b"one\r\ntwo\n\nlast".to_vec(), "a.txt");
        assert_eq!(text.line_starts.len(), 4);
        let lines: Vec<_> = (0..4).map(|l| text.line(l)).collect();
        assert_eq!(lines, ["one", "two", "", "last"]);
        assert_eq!(TextSnapshot::new(b"x\n".to_vec(), "a").line_starts.len(), 1);
        assert_eq!(TextSnapshot::new(Vec::new(), "a").line_starts.len(), 0);
    }
}
