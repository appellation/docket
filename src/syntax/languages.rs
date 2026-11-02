//! The languages Docket can highlight, detected from file paths.
//!
//! Grammars are compiled in; each language's highlights query compiles on
//! first use, since most sessions only touch a few languages.

use std::path::Path;
use std::sync::OnceLock;

use tree_sitter::Query;
use tree_sitter_language::LanguageFn;

use super::HighlightId;

pub struct Language {
    pub name: &'static str,
    grammar: LanguageFn,
    /// Concatenated in order; later patterns win on identical ranges.
    highlights: &'static [&'static str],
    extensions: &'static [&'static str],
    file_names: &'static [&'static str],
    compiled: OnceLock<Option<Compiled>>,
}

pub(super) struct Compiled {
    pub grammar: tree_sitter::Language,
    pub query: Query,
    /// Capture index -> highlight; `None` for captures the theme ignores.
    pub capture_map: Vec<Option<HighlightId>>,
}

impl Language {
    const fn new(
        name: &'static str,
        grammar: LanguageFn,
        highlights: &'static [&'static str],
        extensions: &'static [&'static str],
        file_names: &'static [&'static str],
    ) -> Self {
        Self {
            name,
            grammar,
            highlights,
            extensions,
            file_names,
            compiled: OnceLock::new(),
        }
    }

    /// The grammar and compiled query, or `None` if the query doesn't
    /// compile against the grammar (logged once).
    pub(super) fn compiled(&self) -> Option<&Compiled> {
        self.compiled
            .get_or_init(|| {
                let grammar = tree_sitter::Language::new(self.grammar);
                let source = self.highlights.join("\n");
                match Query::new(&grammar, &source) {
                    Ok(query) => {
                        let capture_map = query
                            .capture_names()
                            .iter()
                            .map(|name| HighlightId::for_capture(name))
                            .collect();
                        Some(Compiled {
                            grammar,
                            query,
                            capture_map,
                        })
                    }
                    Err(err) => {
                        eprintln!("{} highlights query failed to compile: {err}", self.name);
                        None
                    }
                }
            })
            .as_ref()
    }
}

pub struct LanguageRegistry;

static LANGUAGES: [Language; 13] = [
    Language::new(
        "Rust",
        tree_sitter_rust::LANGUAGE,
        &[tree_sitter_rust::HIGHLIGHTS_QUERY],
        &["rs"],
        &[],
    ),
    Language::new(
        "TypeScript",
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        &["ts", "mts", "cts"],
        &[],
    ),
    Language::new(
        "TSX",
        tree_sitter_typescript::LANGUAGE_TSX,
        &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        &["tsx"],
        &[],
    ),
    Language::new(
        "JavaScript",
        tree_sitter_javascript::LANGUAGE,
        &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
        &["js", "jsx", "mjs", "cjs"],
        &[],
    ),
    Language::new(
        "Python",
        tree_sitter_python::LANGUAGE,
        &[tree_sitter_python::HIGHLIGHTS_QUERY],
        &["py", "pyi"],
        &[],
    ),
    Language::new(
        "Go",
        tree_sitter_go::LANGUAGE,
        &[tree_sitter_go::HIGHLIGHTS_QUERY],
        &["go"],
        &[],
    ),
    Language::new(
        "JSON",
        tree_sitter_json::LANGUAGE,
        &[tree_sitter_json::HIGHLIGHTS_QUERY],
        &["json", "jsonc", "json5"],
        &[".prettierrc", ".eslintrc"],
    ),
    Language::new(
        "TOML",
        tree_sitter_toml_ng::LANGUAGE,
        &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
        &["toml"],
        &["Cargo.lock", "uv.lock", "poetry.lock"],
    ),
    Language::new(
        "YAML",
        tree_sitter_yaml::LANGUAGE,
        &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
        &["yml", "yaml"],
        &[],
    ),
    Language::new(
        "Shell",
        tree_sitter_bash::LANGUAGE,
        &[tree_sitter_bash::HIGHLIGHT_QUERY],
        &["sh", "bash", "zsh"],
        &[".bashrc", ".zshrc", ".profile"],
    ),
    Language::new(
        "CSS",
        tree_sitter_css::LANGUAGE,
        &[tree_sitter_css::HIGHLIGHTS_QUERY],
        &["css"],
        &[],
    ),
    Language::new(
        "HTML",
        tree_sitter_html::LANGUAGE,
        &[tree_sitter_html::HIGHLIGHTS_QUERY],
        &["html", "htm"],
        &[],
    ),
    Language::new(
        "Markdown",
        tree_sitter_md::LANGUAGE,
        &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
        &["md", "markdown"],
        &[],
    ),
];

impl LanguageRegistry {
    /// The language for a repository path, by file name then extension.
    pub fn for_path(path: &str) -> Option<&'static Language> {
        let path = Path::new(path);
        let file_name = path.file_name()?.to_str()?;
        if let Some(language) = LANGUAGES.iter().find(|l| l.file_names.contains(&file_name)) {
            return Some(language);
        }
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        LANGUAGES
            .iter()
            .find(|l| l.extensions.contains(&extension.as_str()))
    }

    #[cfg(test)]
    pub fn all() -> &'static [Language] {
        &LANGUAGES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_highlights_query_compiles() {
        for language in LanguageRegistry::all() {
            assert!(language.compiled().is_some(), "{}", language.name);
        }
    }

    #[test]
    fn detects_languages_from_paths() {
        let name = |path| LanguageRegistry::for_path(path).map(|l| l.name);
        assert_eq!(name("src/main.rs"), Some("Rust"));
        assert_eq!(name("web/App.TSX"), Some("TSX"));
        assert_eq!(name("Cargo.lock"), Some("TOML"));
        assert_eq!(name("tsconfig.json"), Some("JSON"));
        assert_eq!(name("README.md"), Some("Markdown"));
        assert_eq!(name("Makefile"), None);
        assert_eq!(name("LICENSE"), None);
    }
}
