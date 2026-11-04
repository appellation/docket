//! A file's symbols as a tree, and the scopes a line sits in.

use std::cmp::Reverse;
use std::ops::Range;

use gpui::SharedString;
use serde::Deserialize;
use serde_json::Value;

use super::client::Position;

/// A range in a `documentSymbol` response. Only lines matter here, plus the
/// start of a symbol's name, which goes back to the server as is.
#[derive(Clone, Copy, Deserialize)]
struct WireRange {
    start: WirePosition,
    end: WirePosition,
}

#[derive(Clone, Copy, Deserialize)]
struct WirePosition {
    line: u32,
    character: u32,
}

impl WireRange {
    /// The lines the range covers. One that ends at the start of a line
    /// doesn't cover that line.
    fn lines(&self) -> Range<u32> {
        let end = if self.end.character == 0 && self.end.line > self.start.line {
            self.end.line
        } else {
            self.end.line + 1
        };
        self.start.line..end
    }

    fn start(&self) -> Position {
        Position {
            line: self.start.line,
            character: self.start.character,
        }
    }
}

/// The protocol's `SymbolKind`, for the kinds Docket names or styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "u32")]
pub enum SymbolKind {
    Module,
    Namespace,
    Package,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    Interface,
    Function,
    Variable,
    Constant,
    /// rust-analyzer reports `impl` blocks as objects.
    Object,
    EnumMember,
    Struct,
    Other,
}

impl From<u32> for SymbolKind {
    fn from(kind: u32) -> Self {
        match kind {
            2 => Self::Module,
            3 => Self::Namespace,
            4 => Self::Package,
            5 => Self::Class,
            6 => Self::Method,
            7 => Self::Property,
            8 => Self::Field,
            9 => Self::Constructor,
            10 => Self::Enum,
            11 => Self::Interface,
            12 => Self::Function,
            13 => Self::Variable,
            14 => Self::Constant,
            19 => Self::Object,
            22 => Self::EnumMember,
            23 => Self::Struct,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    pub name: SharedString,
    /// E.g. a function's signature, as the server phrases it.
    pub detail: Option<SharedString>,
    pub kind: SymbolKind,
    /// The lines it spans, including any doc comment the server counts.
    pub lines: Range<u32>,
    /// Where its name starts, in the server's position encoding: the
    /// position to ask about it.
    pub name_position: Position,
    pub children: Vec<Symbol>,
}

/// A file's symbols, nested by containment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    pub symbols: Vec<Symbol>,
}

impl Outline {
    /// Reads a `textDocument/documentSymbol` response, either nested
    /// `DocumentSymbol`s or flat `SymbolInformation`s.
    pub fn from_response(response: Value) -> serde_json::Result<Self> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct DocumentSymbol {
            name: String,
            detail: Option<String>,
            kind: SymbolKind,
            range: WireRange,
            selection_range: WireRange,
            #[serde(default)]
            children: Vec<DocumentSymbol>,
        }

        #[derive(Deserialize)]
        struct SymbolInformation {
            name: String,
            kind: SymbolKind,
            location: Location,
        }

        #[derive(Deserialize)]
        struct Location {
            range: WireRange,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Response {
            Nested(Vec<DocumentSymbol>),
            Flat(Vec<SymbolInformation>),
        }

        fn convert(symbol: DocumentSymbol) -> Symbol {
            Symbol {
                name: symbol.name.into(),
                detail: symbol
                    .detail
                    .filter(|detail| !detail.is_empty())
                    .map(Into::into),
                kind: symbol.kind,
                lines: symbol.range.lines(),
                name_position: symbol.selection_range.start(),
                children: symbol.children.into_iter().map(convert).collect(),
            }
        }

        let symbols = match serde_json::from_value::<Option<Response>>(response)? {
            None => Vec::new(),
            Some(Response::Nested(symbols)) => symbols.into_iter().map(convert).collect(),
            Some(Response::Flat(symbols)) => nest(
                symbols
                    .into_iter()
                    .map(|symbol| Symbol {
                        name: symbol.name.into(),
                        detail: None,
                        kind: symbol.kind,
                        lines: symbol.location.range.lines(),
                        name_position: symbol.location.range.start(),
                        children: Vec::new(),
                    })
                    .collect(),
            ),
        };
        Ok(Self { symbols })
    }

    /// Every symbol, each before those inside it, with how deep it's
    /// nested.
    pub fn flatten(&self) -> Vec<(usize, &Symbol)> {
        let mut flat = Vec::new();
        let mut stack: Vec<(usize, &Symbol)> = self
            .symbols
            .iter()
            .rev()
            .map(|symbol| (0, symbol))
            .collect();
        while let Some((depth, symbol)) = stack.pop() {
            flat.push((depth, symbol));
            stack.extend(symbol.children.iter().rev().map(|child| (depth + 1, child)));
        }
        flat
    }

    /// The symbols around `line` whose names are above `visible_from`,
    /// outermost first: the scopes a diff excerpt starting at
    /// `visible_from` is in but doesn't show the start of.
    pub fn hidden_scopes(&self, line: u32, visible_from: u32) -> Vec<&Symbol> {
        let mut scopes = Vec::new();
        let mut level = &self.symbols;
        while let Some(symbol) = level
            .iter()
            .find(|symbol| symbol.lines.contains(&line) && symbol.name_position.line < visible_from)
        {
            scopes.push(symbol);
            level = &symbol.children;
        }
        scopes
    }
}

/// Nests flat symbols by the lines they span.
fn nest(mut flat: Vec<Symbol>) -> Vec<Symbol> {
    fn attach(symbol: Symbol, stack: &mut [Symbol], roots: &mut Vec<Symbol>) {
        match stack.last_mut() {
            Some(parent) => parent.children.push(symbol),
            None => roots.push(symbol),
        }
    }

    // Outer symbols sort before the ones they contain.
    flat.sort_by_key(|symbol| (symbol.lines.start, Reverse(symbol.lines.end)));
    let mut roots = Vec::new();
    let mut stack: Vec<Symbol> = Vec::new();
    for symbol in flat {
        while let Some(top) = stack.pop_if(|top| top.lines.end < symbol.lines.end) {
            attach(top, &mut stack, &mut roots);
        }
        stack.push(symbol);
    }
    while let Some(top) = stack.pop() {
        attach(top, &mut stack, &mut roots);
    }
    roots
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn range(start: u32, end: u32) -> Value {
        json!({
            "start": { "line": start, "character": 4 },
            "end": { "line": end, "character": 5 },
        })
    }

    fn document_symbol(name: &str, kind: u32, lines: (u32, u32), children: Value) -> Value {
        json!({
            "name": name,
            "kind": kind,
            "range": range(lines.0, lines.1),
            "selectionRange": range(lines.0, lines.0),
            "children": children,
        })
    }

    fn names(scopes: Vec<&Symbol>) -> Vec<&str> {
        scopes.iter().map(|symbol| symbol.name.as_ref()).collect()
    }

    /// `impl AppState` (lines 10-40) holding `load_diff` (12-20) and
    /// `finish_load` (22-39), as rust-analyzer reports them.
    fn outline() -> Outline {
        Outline::from_response(json!([
            document_symbol("PrFiles", 23, (0, 5), json!([])),
            document_symbol(
                "impl AppState",
                19,
                (10, 40),
                json!([
                    document_symbol("load_diff", 6, (12, 20), json!([])),
                    document_symbol("finish_load", 6, (22, 39), json!([])),
                ])
            ),
        ]))
        .unwrap()
    }

    #[test]
    fn finds_the_scopes_hidden_above_an_excerpt() {
        let outline = outline();
        assert_eq!(
            names(outline.hidden_scopes(15, 14)),
            ["impl AppState", "load_diff"]
        );
        // `finish_load` starts inside the excerpt, so only its `impl` is
        // hidden.
        assert_eq!(names(outline.hidden_scopes(25, 21)), ["impl AppState"]);
        assert!(outline.hidden_scopes(8, 7).is_empty());
        assert!(outline.hidden_scopes(3, 0).is_empty());
    }

    #[test]
    fn flattens_each_symbol_before_its_children() {
        let outline = outline();
        let flat: Vec<_> = outline
            .flatten()
            .into_iter()
            .map(|(depth, symbol)| (depth, symbol.name.as_ref()))
            .collect();
        assert_eq!(
            flat,
            [
                (0, "PrFiles"),
                (0, "impl AppState"),
                (1, "load_diff"),
                (1, "finish_load"),
            ]
        );
    }

    #[test]
    fn reads_symbol_kinds_and_details() {
        let outline = Outline::from_response(json!([{
            "name": "load",
            "detail": "fn(&mut self)",
            "kind": 6,
            "range": range(1, 3),
            "selectionRange": range(1, 1),
        }]))
        .unwrap();
        let symbol = &outline.symbols[0];
        assert_eq!(symbol.kind, SymbolKind::Method);
        assert_eq!(symbol.detail, Some("fn(&mut self)".into()));
        assert_eq!(symbol.lines, 1..4);
        assert_eq!(SymbolKind::from(26), SymbolKind::Other);
        assert_eq!(
            Outline::from_response(Value::Null).unwrap(),
            Outline::default()
        );
    }

    #[test]
    fn ranges_ending_at_a_line_start_exclude_it() {
        let range = |start: (u32, u32), end: (u32, u32)| WireRange {
            start: WirePosition {
                line: start.0,
                character: start.1,
            },
            end: WirePosition {
                line: end.0,
                character: end.1,
            },
        };
        assert_eq!(range((2, 4), (5, 1)).lines(), 2..6);
        assert_eq!(range((2, 4), (5, 0)).lines(), 2..5);
        assert_eq!(range((2, 0), (2, 0)).lines(), 2..3);
    }

    #[test]
    fn nests_flat_symbol_information() {
        let info = |name: &str, kind: u32, lines: (u32, u32)| {
            json!({
                "name": name,
                "kind": kind,
                "location": { "uri": "file:///a.py", "range": range(lines.0, lines.1) },
            })
        };
        let outline = Outline::from_response(json!([
            info("bar", 6, (2, 6)),
            info("Foo", 5, (0, 6)),
            info("y", 13, (3, 4)),
            info("top", 12, (8, 9)),
        ]))
        .unwrap();
        let tree: Vec<_> = outline
            .symbols
            .iter()
            .map(|s| (s.name.as_ref(), s.children.len()))
            .collect();
        assert_eq!(tree, [("Foo", 1), ("top", 0)]);
        assert_eq!(
            outline.symbols[0].children[0].children[0].name.as_ref(),
            "y"
        );
        assert_eq!(names(outline.hidden_scopes(4, 3)), ["Foo", "bar"]);
    }
}
