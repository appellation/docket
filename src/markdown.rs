//! GitHub-flavored Markdown, parsed into blocks the UI can lay out.
//!
//! Parsing is pure and runs on the background executor alongside the fetch;
//! `ui::components::markdown` renders the result. Raw HTML is reduced to its
//! text, and HTML comments (PR templates are full of them) are dropped, as
//! GitHub hides them too. Code blocks in a language Docket knows are
//! highlighted while parsing, so rendering only looks up colors.

use std::ops::Range;

use gpui::SharedString;
use pulldown_cmark::{BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::syntax::{self, HighlightId, Language, LanguageRegistry};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Paragraph(Inline),
    Heading {
        level: u8,
        content: Inline,
    },
    Code(CodeBlock),
    Quote {
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    Table {
        header: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CodeBlock {
    pub text: SharedString,
    /// Sorted, non-overlapping byte ranges into `text`; empty if its
    /// language is unknown.
    pub highlights: Vec<(Range<usize>, HighlightId)>,
}

impl CodeBlock {
    fn new(text: String, language: Option<&Language>) -> Self {
        let highlights = language
            .and_then(|language| syntax::highlight(&text, language))
            .map(|snapshot| {
                snapshot
                    .spans
                    .into_iter()
                    .map(|(range, id)| (range.start as usize..range.end as usize, id))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            text: text.into(),
            highlights,
        }
    }
}

/// GitHub's `> [!NOTE]` callouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListItem {
    /// `Some` for task list items: whether it's checked.
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

/// A run of text with styled ranges and links.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: SharedString,
    /// Sorted, non-overlapping ranges with a non-default style.
    pub runs: Vec<(Range<usize>, InlineStyle)>,
    pub links: Vec<(Range<usize>, String)>,
}

impl Inline {
    pub fn plain(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strikethrough: bool,
    pub link: bool,
}

impl Document {
    pub fn parse(source: &str) -> Self {
        Self::parse_code_in(source, None)
    }

    /// Parses `source`, highlighting code blocks that don't name a
    /// language as `language`: hover docs describe the hovered file's
    /// code, and some servers leave their fences bare.
    pub fn parse_code_in(source: &str, language: Option<&'static Language>) -> Self {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_GFM;
        let mut builder = Builder {
            default_language: language,
            ..Builder::default()
        };
        for event in Parser::new_ext(source, options) {
            builder.event(event);
        }
        builder.finish()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// The first line of text, for one-line previews of collapsed content.
    pub fn preview(&self) -> Option<String> {
        fn first(blocks: &[Block]) -> Option<String> {
            blocks.iter().find_map(|block| match block {
                Block::Paragraph(inline)
                | Block::Heading {
                    content: inline, ..
                } => inline
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_owned),
                Block::Quote { blocks, .. } => first(blocks),
                Block::List { items, .. } => items.iter().find_map(|i| first(&i.blocks)),
                _ => None,
            })
        }
        first(&self.blocks)
    }
}

/// An open block that collects children.
enum Container {
    Root(Vec<Block>),
    Quote(Option<Alert>, Vec<Block>),
    List(Option<u64>, Vec<ListItem>),
    Item(ListItem),
    Table {
        header: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
        row: Vec<Inline>,
    },
}

/// The paragraph, heading, or table cell receiving text.
#[derive(Default)]
struct InlineBuilder {
    text: String,
    runs: Vec<(Range<usize>, InlineStyle)>,
    links: Vec<(Range<usize>, String)>,
}

impl InlineBuilder {
    fn push(&mut self, text: &str, style: InlineStyle) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        if style == InlineStyle::default() {
            return;
        }
        match self.runs.last_mut() {
            Some((range, last)) if range.end == start && *last == style => range.end = end,
            _ => self.runs.push((start..end, style)),
        }
    }

    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    fn finish(self) -> Inline {
        // Trailing line breaks (before a nested list, say) only add height.
        let len = self.text.trim_end().len();
        let mut text = self.text;
        text.truncate(len);
        let clamp = |r: Range<usize>| r.start.min(len)..r.end.min(len);
        Inline {
            runs: self
                .runs
                .into_iter()
                .map(|(r, s)| (clamp(r), s))
                .filter(|(r, _)| !r.is_empty())
                .collect(),
            links: self
                .links
                .into_iter()
                .map(|(r, url)| (clamp(r), url))
                .filter(|(r, _)| !r.is_empty())
                .collect(),
            text: text.into(),
        }
    }
}

struct Builder {
    stack: Vec<Container>,
    inline: Option<InlineBuilder>,
    heading: Option<u8>,
    /// The open code block's text, and the language to highlight it as.
    code: Option<(String, Option<&'static Language>)>,
    /// The language of code blocks that don't name one.
    default_language: Option<&'static Language>,
    html: Option<String>,
    in_html_comment: bool,
    in_table_head: bool,
    bold: u32,
    italic: u32,
    strikethrough: u32,
    /// Where the open link's text starts, and its target.
    link: Option<(usize, String)>,
    /// Whether the open link is an image, whose text is its alt text.
    in_image: bool,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            stack: vec![Container::Root(Vec::new())],
            inline: None,
            heading: None,
            code: None,
            default_language: None,
            html: None,
            in_html_comment: false,
            in_table_head: false,
            bold: 0,
            italic: 0,
            strikethrough: 0,
            link: None,
            in_image: false,
        }
    }
}

impl Builder {
    fn style(&self) -> InlineStyle {
        InlineStyle {
            bold: self.bold > 0,
            italic: self.italic > 0,
            code: false,
            strikethrough: self.strikethrough > 0,
            link: self.link.is_some(),
        }
    }

    fn inline(&mut self) -> &mut InlineBuilder {
        self.inline.get_or_insert_default()
    }

    /// Ends the open paragraph, if any. Tight list items have no paragraph
    /// events, so text opens one implicitly.
    fn flush(&mut self) {
        if let Some(inline) = self.inline.take()
            && !inline.is_blank()
        {
            self.push_block(Block::Paragraph(inline.finish()));
        }
    }

    fn push_block(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(Container::Root(blocks) | Container::Quote(_, blocks)) => blocks.push(block),
            Some(Container::Item(item)) => item.blocks.push(block),
            // Blocks can't sit directly in a list or table; GitHub wouldn't
            // produce that, but don't lose the content if it happens.
            Some(Container::List(_, items)) => items.push(ListItem {
                checked: None,
                blocks: vec![block],
            }),
            Some(Container::Table { .. }) | None => {}
        }
    }

    fn text(&mut self, text: &str) {
        if let Some((code, _)) = &mut self.code {
            code.push_str(text);
            return;
        }
        let style = self.style();
        if self.link.is_some() {
            self.inline().push(text, style);
            return;
        }
        // GitHub links bare URLs; CommonMark doesn't.
        let mut rest = text;
        while let Some((before, url, after)) = find_url(rest) {
            self.inline().push(before, style);
            let inline = self.inline();
            let start = inline.text.len();
            inline.push(
                url,
                InlineStyle {
                    link: true,
                    ..style
                },
            );
            inline
                .links
                .push((start..inline.text.len(), url.to_owned()));
            rest = after;
        }
        self.inline().push(rest, style);
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => {
                let style = InlineStyle {
                    code: true,
                    ..self.style()
                };
                self.inline().push(&code, style);
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                let style = InlineStyle {
                    code: true,
                    ..self.style()
                };
                self.inline().push(&math, style);
            }
            Event::Html(html) => {
                if let Some(buffer) = &mut self.html {
                    buffer.push_str(&html);
                }
            }
            Event::InlineHtml(html) => {
                let text = strip_html(&html, &mut self.in_html_comment);
                if !text.is_empty() {
                    let style = self.style();
                    self.inline().push(&text, style);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                // GitHub keeps single line breaks in comments.
                let style = self.style();
                self.inline().push("\n", style);
            }
            Event::Rule => {
                self.flush();
                self.push_block(Block::Rule);
            }
            Event::TaskListMarker(checked) => {
                if let Some(Container::Item(item)) = self.stack.last_mut() {
                    item.checked = Some(checked);
                }
            }
            Event::FootnoteReference(_) => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.flush(),
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level as u8);
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                // An info string like `rust,ignore` or `ts title="a.ts"`
                // starts with the language. One Docket doesn't know stays
                // plain rather than falling back.
                let info = match &kind {
                    CodeBlockKind::Fenced(info) => info.split([' ', ',', '{']).next(),
                    CodeBlockKind::Indented => None,
                };
                let language = match info.filter(|name| !name.is_empty()) {
                    Some(name) => LanguageRegistry::for_name(name),
                    None => self.default_language,
                };
                self.code = Some((String::new(), language));
            }
            Tag::HtmlBlock => {
                self.flush();
                self.html = Some(String::new());
            }
            Tag::BlockQuote(kind) => {
                self.flush();
                let alert = kind.map(|kind| match kind {
                    BlockQuoteKind::Note => Alert::Note,
                    BlockQuoteKind::Tip => Alert::Tip,
                    BlockQuoteKind::Important => Alert::Important,
                    BlockQuoteKind::Warning => Alert::Warning,
                    BlockQuoteKind::Caution => Alert::Caution,
                });
                self.stack.push(Container::Quote(alert, Vec::new()));
            }
            Tag::List(start) => {
                self.flush();
                self.stack.push(Container::List(start, Vec::new()));
            }
            Tag::Item => {
                self.flush();
                self.stack.push(Container::Item(ListItem::default()));
            }
            Tag::Table(_) => {
                self.flush();
                self.stack.push(Container::Table {
                    header: Vec::new(),
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Tag::TableHead => self.in_table_head = true,
            Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strikethrough += 1,
            Tag::Link { dest_url, .. } => {
                let start = self.inline().text.len();
                self.link = Some((start, dest_url.into_string()));
            }
            Tag::Image { dest_url, .. } => {
                let start = self.inline().text.len();
                self.link = Some((start, dest_url.into_string()));
                self.in_image = true;
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                let content = self.inline.take().unwrap_or_default().finish();
                let level = self.heading.take().unwrap_or(1);
                self.push_block(Block::Heading { level, content });
            }
            TagEnd::CodeBlock => {
                let (mut code, language) = self.code.take().unwrap_or_default();
                code.truncate(code.trim_end_matches('\n').len());
                self.push_block(Block::Code(CodeBlock::new(code, language)));
            }
            TagEnd::HtmlBlock => {
                let html = self.html.take().unwrap_or_default();
                let text = strip_html(&html, &mut self.in_html_comment);
                let text = collapse_blank_lines(&text);
                if !text.is_empty() {
                    self.push_block(Block::Paragraph(Inline::plain(text)));
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                if let Some(Container::Quote(alert, blocks)) = self.stack.pop() {
                    self.push_block(Block::Quote { alert, blocks });
                }
            }
            TagEnd::List(_) => {
                self.flush();
                if let Some(Container::List(start, items)) = self.stack.pop() {
                    self.push_block(Block::List { start, items });
                }
            }
            TagEnd::Item => {
                self.flush();
                if let Some(Container::Item(item)) = self.stack.pop()
                    && let Some(Container::List(_, items)) = self.stack.last_mut()
                {
                    items.push(item);
                }
            }
            TagEnd::TableCell => {
                let cell = self.inline.take().unwrap_or_default().finish();
                if let Some(Container::Table { row, .. }) = self.stack.last_mut() {
                    row.push(cell);
                }
            }
            TagEnd::TableHead => {
                self.in_table_head = false;
                if let Some(Container::Table { header, row, .. }) = self.stack.last_mut() {
                    *header = std::mem::take(row);
                }
            }
            TagEnd::TableRow => {
                if let Some(Container::Table { rows, row, .. }) = self.stack.last_mut() {
                    rows.push(std::mem::take(row));
                }
            }
            TagEnd::Table => {
                if let Some(Container::Table { header, rows, .. }) = self.stack.pop() {
                    self.push_block(Block::Table { header, rows });
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strikethrough = self.strikethrough.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => {
                let alt_start = self.link.as_ref().map(|(start, _)| *start);
                // An image with no alt text still needs something to click.
                if std::mem::take(&mut self.in_image) && alt_start == Some(self.inline().text.len())
                {
                    let style = self.style();
                    self.inline().push("image", style);
                }
                if let Some((start, url)) = self.link.take() {
                    let inline = self.inline();
                    let end = inline.text.len();
                    inline.links.push((start..end, url));
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Document {
        self.flush();
        // Close anything left open by malformed input.
        while self.stack.len() > 1 {
            match self.stack.pop() {
                Some(Container::Quote(alert, blocks)) => {
                    self.push_block(Block::Quote { alert, blocks })
                }
                Some(Container::List(start, items)) => {
                    self.push_block(Block::List { start, items })
                }
                Some(Container::Item(item)) => {
                    if let Some(Container::List(_, items)) = self.stack.last_mut() {
                        items.push(item);
                    }
                }
                Some(Container::Table { header, rows, .. }) => {
                    self.push_block(Block::Table { header, rows })
                }
                Some(Container::Root(_)) | None => {}
            }
        }
        match self.stack.pop() {
            Some(Container::Root(blocks)) => Document { blocks },
            _ => Document::default(),
        }
    }
}

/// Finds the first `http(s)://` URL in `text`: (before, url, after).
fn find_url(text: &str) -> Option<(&str, &str, &str)> {
    let start = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| text.find(scheme))
        .min()?;
    let rest = &text[start..];
    let mut end = rest
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>')
        .unwrap_or(rest.len());
    // Sentence punctuation after a URL isn't part of it; a closing paren is
    // only when the URL opened one.
    while let Some(c) = rest[..end].chars().next_back() {
        let unbalanced_paren = c == ')' && !rest[..end].contains('(');
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"') || unbalanced_paren {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    let url = &rest[..end];
    if url.len() <= "https://".len() {
        // Just a scheme; look past it.
        let skip = start + url.len().max(1);
        let (before, url, after) = find_url(&text[skip..])?;
        return Some((&text[..skip + before.len()], url, after));
    }
    Some((&text[..start], url, &rest[end..]))
}

/// Reduces HTML to its text. `in_comment` carries an unterminated `<!--`
/// across calls, since comments can span several HTML events.
fn strip_html(html: &str, in_comment: &mut bool) -> String {
    let mut out = String::new();
    let mut rest = html;
    loop {
        if *in_comment {
            match rest.find("-->") {
                Some(end) => {
                    rest = &rest[end + 3..];
                    *in_comment = false;
                }
                None => break,
            }
        }
        let Some(open) = rest.find('<') else {
            out.push_str(&decode_entities(rest));
            break;
        };
        out.push_str(&decode_entities(&rest[..open]));
        rest = &rest[open..];
        if rest.starts_with("<!--") {
            *in_comment = true;
            rest = &rest[4..];
            continue;
        }
        let Some(close) = rest.find('>') else {
            // Not a tag after all.
            out.push_str(&decode_entities(rest));
            break;
        };
        let tag = rest[1..close].trim().to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        let closing = tag.starts_with('/');
        let breaks = match name {
            "br" => true,
            "p" | "div" | "li" | "tr" | "summary" | "details" | "table" | "ul" | "ol" => closing,
            n if n.len() == 2 && n.starts_with('h') => closing,
            _ => false,
        };
        if breaks {
            out.push('\n');
        }
        rest = &rest[close + 1..];
    }
    out
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn collapse_blank_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(doc: &Document, ix: usize) -> &Inline {
        match &doc.blocks[ix] {
            Block::Paragraph(inline) => inline,
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    #[test]
    fn styles_inline_text() {
        let doc = Document::parse("Some **bold** and `code` and [a link](https://x.dev).");
        let p = paragraph(&doc, 0);
        assert_eq!(p.text.as_ref(), "Some bold and code and a link.");
        let styled: Vec<_> = p
            .runs
            .iter()
            .map(|(r, s)| (&p.text[r.clone()], *s))
            .collect();
        assert_eq!(
            styled,
            [
                (
                    "bold",
                    InlineStyle {
                        bold: true,
                        ..Default::default()
                    }
                ),
                (
                    "code",
                    InlineStyle {
                        code: true,
                        ..Default::default()
                    }
                ),
                (
                    "a link",
                    InlineStyle {
                        link: true,
                        ..Default::default()
                    }
                ),
            ]
        );
        assert_eq!(p.links, [(23..29, "https://x.dev".to_owned())]);
    }

    #[test]
    fn links_bare_urls() {
        let doc = Document::parse("See https://github.com/a/b/pull/1. Thanks");
        let p = paragraph(&doc, 0);
        let (range, url) = &p.links[0];
        assert_eq!(url, "https://github.com/a/b/pull/1");
        assert_eq!(&p.text[range.clone()], url);
    }

    #[test]
    fn drops_html_comments() {
        let doc = Document::parse(
            "<!-- Describe your change.\n\nMore template text. -->\n\nFixes the bug.",
        );
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(paragraph(&doc, 0).text.as_ref(), "Fixes the bug.");
    }

    #[test]
    fn keeps_text_of_html_blocks() {
        let doc =
            Document::parse("<details>\n<summary>Bundle size</summary>\n\n12 kB\n\n</details>");
        let texts: Vec<_> = doc
            .blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph(p) => p.text.to_string(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(texts, ["Bundle size", "12 kB"]);
    }

    #[test]
    fn parses_task_lists_and_nesting() {
        let doc = Document::parse("- [x] done\n- [ ] todo\n  - nested");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("expected a list");
        };
        assert_eq!(items[0].checked, Some(true));
        assert_eq!(items[1].checked, Some(false));
        assert!(matches!(items[1].blocks[1], Block::List { .. }));
    }

    #[test]
    fn parses_alerts_and_code() {
        let doc = Document::parse("> [!WARNING]\n> Careful\n\n```rust\nfn main() {}\n```");
        assert!(matches!(
            doc.blocks[0],
            Block::Quote {
                alert: Some(Alert::Warning),
                ..
            }
        ));
        let Block::Code(code) = &doc.blocks[1] else {
            panic!("expected code, got {:?}", doc.blocks[1]);
        };
        assert_eq!(code.text, "fn main() {}");
    }

    /// The highlight names in `block`'s code, by the text they cover.
    fn highlights(block: &Block) -> Vec<(&str, &'static str)> {
        let Block::Code(code) = block else {
            panic!("expected code, got {block:?}");
        };
        code.highlights
            .iter()
            .map(|(range, id)| (&code.text[range.clone()], id.name()))
            .collect()
    }

    #[test]
    fn highlights_code_in_named_languages() {
        let doc = Document::parse("```rust,ignore\nfn main() {}\n```\n\n```text\nfn x\n```");
        assert!(highlights(&doc.blocks[0]).contains(&("fn", "keyword")));
        assert!(highlights(&doc.blocks[0]).contains(&("main", "function")));
        assert_eq!(highlights(&doc.blocks[1]), []);
    }

    #[test]
    fn highlights_bare_code_in_the_default_language() {
        let rust = LanguageRegistry::for_name("rust");
        let source = "```\nfn a()\n```\n\n```text\nfn b()\n```";
        let doc = Document::parse_code_in(source, rust);
        assert!(highlights(&doc.blocks[0]).contains(&("fn", "keyword")));
        assert_eq!(highlights(&doc.blocks[1]), []);
        assert_eq!(highlights(&Document::parse(source).blocks[0]), []);
    }

    #[test]
    fn previews_first_line() {
        let doc = Document::parse("## Summary\n\nDetails");
        assert_eq!(doc.preview().as_deref(), Some("Summary"));
    }
}
