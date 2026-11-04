//! Searching the browsed files from the tree's sidebar: by name, by
//! contents, or for a language server's symbols. One query, searched in
//! whichever mode is chosen; switching modes searches it again.
//!
//! Names and contents search the tree's [`FileIndex`], built the first
//! time a search needs it and again after each listing (a refresh, or the
//! pull request's head moving). The searching itself is in
//! `projects::search`. Symbols come from `workspace/symbol`, asked of every
//! running server for the project's languages; choosing the mode starts
//! them, and the search runs again as each becomes ready, since what a
//! server knows grows as it indexes.
//!
//! Each new query replaces the search in flight: its task is dropped, and
//! a content search on other threads is told to stop. Contents and symbols
//! wait a moment (`SEARCH_DEBOUNCE`) for typing to pause first. Results
//! stay shown until the next ones arrive.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::anyhow;
use futures::future::{Either, join_all, select};
use gpui::{Context, SharedString, Task};

use super::code_intel::ServerState;
use super::{BrowserKey, Tab};
use crate::lsp::{LanguageServer, Location, PositionEncoding, SymbolKind, server_for};
use crate::projects::search::{
    self, ContentQuery, ContentResults, FileIndex, fuzzy_find, search_contents,
};

/// How long contents and symbols wait for typing to pause.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// Files a name search shows, at most.
pub const FILE_RESULTS: usize = 200;
/// Matching lines a content search shows, at most.
pub const CONTENT_RESULTS: usize = 2_000;
/// Symbols a symbol search shows, at most.
pub const SYMBOL_RESULTS: usize = 200;
/// Longest to wait for a server's symbols.
const SYMBOL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchMode {
    /// Files by name, fuzzily.
    #[default]
    Files,
    /// Lines of the files' text.
    Contents,
    /// Symbols the language servers know.
    Symbols,
}

#[derive(Default)]
pub struct BrowserSearch {
    pub mode: SearchMode,
    pub query: String,
    /// Contents: the query is a regex.
    pub regex: bool,
    /// Contents: match case even when the query is all lowercase.
    pub case_sensitive: bool,
    index: Option<IndexLoad>,
    pub results: SearchResults,
    /// A search is running; `results` are the last one's.
    pub searching: bool,
    /// Bumped whenever `results` change, so views know to rebuild rows.
    pub revision: u64,
    _index_task: Option<Task<()>>,
    _task: Option<Task<()>>,
    cancel: Option<Cancel>,
}

impl BrowserSearch {
    /// Whether there's a query to search for, so results show in place of
    /// the tree.
    pub fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn content_query(&self) -> ContentQuery {
        ContentQuery {
            text: self.query.clone(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
        }
    }
}

enum IndexLoad {
    Loading,
    Ready(Arc<FileIndex>),
    Failed(SharedString),
}

#[derive(Default)]
pub enum SearchResults {
    #[default]
    None,
    Files(Arc<Vec<FileResult>>),
    Contents(Arc<ContentResults>),
    Symbols {
        symbols: Arc<Vec<SymbolResult>>,
        /// Why there may be fewer than there are: a server still
        /// loading, or none to ask.
        note: Option<SharedString>,
    },
    Failed(SharedString),
}

impl SearchResults {
    /// Whether these are a search in `mode`'s (or say why it failed).
    pub fn is_for(&self, mode: SearchMode) -> bool {
        matches!(
            (self, mode),
            (Self::Files(_), SearchMode::Files)
                | (Self::Contents(_), SearchMode::Contents)
                | (Self::Symbols { .. }, SearchMode::Symbols)
                | (Self::Failed(_), _)
        )
    }
}

/// A file whose path matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileResult {
    pub path: String,
    /// Byte offsets of the matched characters in `path`.
    pub positions: Vec<usize>,
}

/// A symbol in the browsed checkout.
#[derive(Clone, Debug, PartialEq)]
pub struct SymbolResult {
    pub name: String,
    pub kind: SymbolKind,
    pub container: Option<String>,
    /// Relative to the checkout.
    pub path: String,
    pub location: Location,
    /// How the server counts characters in `location`.
    pub encoding: PositionEncoding,
    /// Byte offsets of the matched characters in `name`.
    pub positions: Vec<usize>,
}

/// Tells a content search on other threads to stop when the search it's
/// for is replaced.
struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Tab {
    pub fn set_search_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        if browser.search.query == query {
            return;
        }
        browser.search.query = query.to_owned();
        self.run_search(cx);
    }

    pub fn set_search_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        if browser.search.mode == mode {
            return;
        }
        browser.search.mode = mode;
        self.run_search(cx);
    }

    pub fn toggle_search_regex(&mut self, cx: &mut Context<Self>) {
        if let Some(browser) = &mut self.browser {
            browser.search.regex = !browser.search.regex;
            self.run_search(cx);
        }
    }

    pub fn toggle_search_case(&mut self, cx: &mut Context<Self>) {
        if let Some(browser) = &mut self.browser {
            browser.search.case_sensitive = !browser.search.case_sensitive;
            self.run_search(cx);
        }
    }

    /// Readies the search while a query is still to be typed: builds the
    /// index, and for contents, finds which files Git can read.
    pub fn prepare_search(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        match &browser.search.index {
            None => self.load_search_index(cx),
            Some(IndexLoad::Ready(index)) if browser.search.mode == SearchMode::Contents => {
                let (index, tree) = (index.clone(), browser.tree.clone());
                cx.background_executor()
                    .spawn(async move {
                        if let Some(tree) = tree {
                            index.prepare_contents(&tree);
                        }
                    })
                    .detach();
            }
            _ => {}
        }
    }

    /// The tree was listed again, so the index is out of date: build it
    /// again, and search it again if there's a query.
    pub(super) fn search_tree_changed(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        browser.search.index = None;
        browser.search._index_task = None;
        if browser.search.is_active() {
            self.run_search(cx);
        }
    }

    /// A browsed checkout's server started, failed, or finished loading:
    /// what it knows changed, so search symbols again.
    pub(super) fn search_servers_changed(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &self.browser else {
            return;
        };
        if browser.search.mode == SearchMode::Symbols && browser.search.is_active() {
            self.run_search(cx);
        }
    }

    /// Searches for the query in the chosen mode, replacing any search in
    /// flight.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let search = &mut browser.search;
        search._task = None;
        search.cancel = None;
        if !search.results.is_for(search.mode) {
            search.results = SearchResults::None;
            search.revision += 1;
        }
        if !search.is_active() {
            search.results = SearchResults::None;
            search.searching = false;
            search.revision += 1;
            cx.notify();
            return;
        }
        search.searching = true;
        cx.notify();
        let index = match &search.index {
            Some(IndexLoad::Ready(index)) => index.clone(),
            Some(IndexLoad::Loading) => return,
            Some(IndexLoad::Failed(err)) => {
                let err = err.clone();
                self.finish_search(SearchResults::Failed(err), cx);
                return;
            }
            None => {
                self.load_search_index(cx);
                return;
            }
        };
        match self.browser.as_ref().map(|browser| browser.search.mode) {
            Some(SearchMode::Files) => self.search_files(index, cx),
            Some(SearchMode::Contents) => self.search_contents(index, cx),
            Some(SearchMode::Symbols) => self.search_symbols(index, cx),
            None => {}
        }
    }

    /// Builds the index, then searches it. Until the root is listed there's
    /// no tree to index; the listing searches again once it's done.
    fn load_search_index(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let Some(tree) = browser.tree.clone() else {
            return;
        };
        let key = browser.key();
        browser.search.index = Some(IndexLoad::Loading);
        browser.search._index_task = Some(cx.spawn(async move |this, cx| {
            let index = cx
                .background_executor()
                .spawn(async move { tree.index() })
                .await;
            this.update(cx, |this, cx| {
                let Some(browser) = this.browser_for(&key) else {
                    return;
                };
                browser.search.index = Some(match index {
                    Ok(index) => IndexLoad::Ready(Arc::new(index)),
                    Err(err) => IndexLoad::Failed(format!("{err:#}").into()),
                });
                this.run_search(cx);
            })
            .ok();
        }));
    }

    fn browser_for(&mut self, key: &BrowserKey) -> Option<&mut super::ProjectBrowser> {
        self.browser
            .as_mut()
            .filter(|browser| browser.key() == *key)
    }

    /// Shows a search's results, if the browser is still the one searched.
    fn finish_search(&mut self, results: SearchResults, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let search = &mut browser.search;
        search.results = results;
        search.searching = false;
        search.revision += 1;
        cx.notify();
    }

    /// Matches paths as they're typed; it's quick enough not to wait.
    fn search_files(&mut self, index: Arc<FileIndex>, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let query = browser.search.query.clone();
        let key = browser.key();
        browser.search._task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    fuzzy_find(&index.files, &query, true, FILE_RESULTS)
                        .into_iter()
                        .map(|found| FileResult {
                            path: index.files[found.index].clone(),
                            positions: found.positions,
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                if this.browser_for(&key).is_some() {
                    this.finish_search(SearchResults::Files(Arc::new(found)), cx);
                }
            })
            .ok();
        }));
    }

    fn search_contents(&mut self, index: Arc<FileIndex>, cx: &mut Context<Self>) {
        let Some(browser) = &mut self.browser else {
            return;
        };
        let Some(tree) = browser.tree.clone() else {
            return;
        };
        let query = browser.search.content_query();
        let key = browser.key();
        let cancel = Arc::new(AtomicBool::new(false));
        browser.search.cancel = Some(Cancel(cancel.clone()));
        browser.search._task = Some(cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            background.timer(SEARCH_DEBOUNCE).await;
            let found = background
                .spawn(
                    async move { search_contents(&tree, &index, &query, CONTENT_RESULTS, &cancel) },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.browser_for(&key).is_none() {
                    return;
                }
                let results = match found {
                    Ok(found) => SearchResults::Contents(Arc::new(found)),
                    Err(err) => SearchResults::Failed(format!("{err:#}").into()),
                };
                this.finish_search(results, cx);
            })
            .ok();
        }));
    }

    /// Starts the servers for the project's languages, then asks those
    /// running for symbols matching the query, ranked as names are.
    fn search_symbols(&mut self, index: Arc<FileIndex>, cx: &mut Context<Self>) {
        let Some(root) = self.browser_root(cx) else {
            let note = "The files aren't listed yet.";
            self.finish_search(SearchResults::Failed(note.into()), cx);
            return;
        };
        let languages: Vec<_> = index
            .languages
            .iter()
            .map(|(language, _)| *language)
            .filter(|language| server_for(language).is_some())
            .collect();
        for language in &languages {
            self.start_browser_server(language, cx);
        }
        let slots = self
            .browser_code_intel()
            .map(|intel| intel.servers.as_slice())
            .unwrap_or_default();
        let servers: Vec<Arc<LanguageServer>> = slots
            .iter()
            .filter_map(|slot| match &slot.state {
                ServerState::Running { server, .. } if server.searches_symbols() => {
                    Some(server.clone())
                }
                _ => None,
            })
            .collect();
        let loading: Vec<&str> = slots
            .iter()
            .filter(|slot| match &slot.state {
                ServerState::Starting => true,
                ServerState::Running { activity, .. } => activity.busy,
                _ => false,
            })
            .map(|slot| slot.spec.name)
            .collect();
        let note: Option<SharedString> = if languages.is_empty() {
            Some("Docket has no language server for this project's languages.".into())
        } else if !loading.is_empty() {
            Some(
                format!(
                    "{} is still loading, so some may be missing.",
                    loading.join(", ")
                )
                .into(),
            )
        } else if servers.is_empty() {
            Some("No language server for this project is running.".into())
        } else {
            None
        };
        if servers.is_empty() {
            let results = SearchResults::Symbols {
                symbols: Arc::new(Vec::new()),
                note,
            };
            // Searched again once a server starts.
            let starting = !loading.is_empty();
            self.finish_search(results, cx);
            if let Some(browser) = &mut self.browser {
                browser.search.searching = starting;
            }
            return;
        }

        let Some(browser) = &mut self.browser else {
            return;
        };
        let query = browser.search.query.clone();
        let key = browser.key();
        browser.search._task = Some(cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            background.timer(SEARCH_DEBOUNCE).await;
            let requests = servers.iter().map(|server| {
                let encoding = server.encoding;
                let request = server.workspace_symbols(&query);
                let timeout = background.timer(SYMBOL_TIMEOUT);
                async move {
                    let found = match select(Box::pin(request), timeout).await {
                        Either::Left((found, _)) => found,
                        Either::Right(_) => Err(anyhow!("no answer within 10 seconds")),
                    };
                    (encoding, found)
                }
            });
            let answers = join_all(requests).await;
            let ranked = background
                .spawn(async move { rank_symbols(answers, &root, &query) })
                .await;
            this.update(cx, |this, cx| {
                if this.browser_for(&key).is_none() {
                    return;
                }
                let results = match ranked {
                    Ok(symbols) => SearchResults::Symbols {
                        symbols: Arc::new(symbols),
                        note,
                    },
                    Err(err) => SearchResults::Failed(format!("{err:#}").into()),
                };
                this.finish_search(results, cx);
            })
            .ok();
        }));
    }
}

/// The servers' symbols that are in the checkout, best match first. Those
/// a server matched that don't match as names do (servers match more
/// loosely) follow, in its order. Fails only if every server did.
fn rank_symbols(
    answers: Vec<(
        PositionEncoding,
        anyhow::Result<Vec<crate::lsp::WorkspaceSymbol>>,
    )>,
    root: &Path,
    query: &str,
) -> anyhow::Result<Vec<SymbolResult>> {
    // Servers may answer with the root's real path.
    let roots: Vec<PathBuf> = [Some(root.to_owned()), root.canonicalize().ok()]
        .into_iter()
        .flatten()
        .collect();
    let mut failure = None;
    let mut symbols = Vec::new();
    let any_answered = answers.iter().any(|(_, found)| found.is_ok());
    for (encoding, found) in answers {
        let found = match found {
            Ok(found) => found,
            Err(err) => {
                failure = Some(err);
                continue;
            }
        };
        for symbol in found {
            let Some(path) = roots
                .iter()
                .find_map(|root| relative_path(&symbol.location.path, root))
            else {
                continue;
            };
            symbols.push(SymbolResult {
                name: symbol.name,
                kind: symbol.kind,
                container: symbol.container,
                path,
                location: symbol.location,
                encoding,
                positions: Vec::new(),
            });
        }
    }
    if !any_answered && let Some(err) = failure {
        return Err(err);
    }
    let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    let matched = fuzzy_find(&names, query, false, SYMBOL_RESULTS);
    let mut taken = vec![false; symbols.len()];
    let mut ranked = Vec::with_capacity(symbols.len().min(SYMBOL_RESULTS));
    for found in matched {
        taken[found.index] = true;
        ranked.push(SymbolResult {
            positions: found.positions,
            ..symbols[found.index].clone()
        });
    }
    ranked.extend(
        symbols
            .into_iter()
            .zip(taken)
            .filter(|(_, taken)| !taken)
            .map(|(symbol, _)| symbol),
    );
    ranked.truncate(SYMBOL_RESULTS);
    Ok(ranked)
}

/// `path` relative to `root`, with forward slashes, if it's inside it.
fn relative_path(path: &Path, root: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    (!relative.is_empty()).then_some(relative)
}

/// Highlight ranges for a result's matched characters.
pub fn matched_ranges(text: &str, positions: &[usize]) -> Vec<std::ops::Range<usize>> {
    search::position_ranges(text, positions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::{Position, WorkspaceSymbol};

    #[test]
    fn ranks_symbols_in_the_checkout() {
        let at = |path: &str, line| Location {
            path: PathBuf::from(path),
            range: (
                Position { line, character: 3 },
                Position { line, character: 9 },
            ),
            extent: (
                Position { line, character: 3 },
                Position { line, character: 9 },
            ),
        };
        let symbol = |name: &str, path: &str| WorkspaceSymbol {
            name: name.to_owned(),
            kind: SymbolKind::Function,
            container: None,
            location: at(path, 4),
        };
        let answers = vec![
            (
                PositionEncoding::Utf16,
                Ok(vec![
                    symbol("render_widget", "/work/src/ui.rs"),
                    symbol("Widget", "/work/src/widget.rs"),
                    symbol("Widget", "/rustlib/core/widget.rs"),
                    symbol("wdgt_helper", "/work/src/helpers.rs"),
                ]),
            ),
            (PositionEncoding::Utf8, Err(anyhow!("it exited"))),
        ];
        let ranked = rank_symbols(answers, Path::new("/work"), "widget").unwrap();
        let names: Vec<_> = ranked
            .iter()
            .map(|s| (s.name.as_str(), s.path.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("Widget", "src/widget.rs"),
                ("render_widget", "src/ui.rs"),
                // The server's own match, though it isn't one here.
                ("wdgt_helper", "src/helpers.rs"),
            ]
        );
        assert_eq!(ranked[0].positions, [0, 1, 2, 3, 4, 5]);
        assert!(ranked[2].positions.is_empty());

        let failed = vec![(PositionEncoding::Utf8, Err(anyhow!("it exited")))];
        assert!(rank_symbols(failed, Path::new("/work"), "w").is_err());
    }
}
