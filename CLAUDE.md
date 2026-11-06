# Docket

Docket is a native desktop app (Rust + GPUI) for managing GitHub pull requests,
the ones you're reviewing and the ones you're shipping. See `README.md` for
setup, auth, and what the app does from a user's point of view; this file
covers the intent behind the project so changes stay pointed in the right
direction, and how the code is put together.

## The name

A docket is a court's list of pending cases. The app is meant to be your list
of pending PRs: everything waiting on you to review, and everything of yours
that is still in flight. Product decisions should reinforce that framing. Docket
answers "what needs my attention, and what's the state of my work?" before it
answers anything else.

Use "Docket" (capitalized) in user-facing text and `docket` for identifiers
(crate name, actions namespace, `dev.docket` keychain service,
`DOCKET_CLIENT_ID`, user agent).

## Goals

Docket aims to be a fully managed PR triage app, replacing the GitHub web UI
and notification email for day-to-day PR work:

- **Triage.** Surface PRs that need action (review requested, changes
  requested on yours, CI failing, ready to merge) and make it fast to decide
  what to do next. Signal over completeness.
- **In-progress tracking.** Keep track of your own open and draft PRs: review
  state, checks, unresolved threads, and what's blocking each one.
- **Ergonomic review flow.** Keyboard-driven review that beats the web UI:
  moving between files and hunks, commenting, tracking what you've already
  viewed, and submitting reviews without context switching.
- **Editor integration.** Open a PR's files or a specific line in the user's
  editor, and check out PR branches locally.
- **LSP support.** Code intelligence (go to definition, hover, references) when
  reading diffs, so reviewers can understand changes without leaving the app.
- **Notifications.** Native, actionable notifications for events that matter
  (new review requests, reviews on your PRs, CI results, mentions). Prefer
  fewer, higher-signal notifications to mirroring GitHub's firehose.
- **Organization.** Saved searches, repo and team filters, grouping, snoozing
  and pinning, so users can shape the docket around how they work.

## Principles

- **No backend.** The app talks to GitHub directly using the user's OAuth token
  (device flow). Don't introduce a server dependency for a feature; local
  state and the GitHub API should be enough.
- **Native and fast.** Lean on GPUI for a responsive, keyboard-first UI. Never
  block the UI thread on network or disk; follow the background-executor
  pattern under Conventions below.
- **GraphQL first.** Fetch GitHub data via GraphQL where possible, requesting
  only what the view needs.

## Status

Early scaffold: sign-in, projects (local checkouts connected to their GitHub
repository, which scope the docket), the docket (PRs waiting on your review,
then your other PRs and local branches by last update), a PR detail view
showing the conversation and diff, a branch's diff against the default
branch (grouped by change by Claude when Anthropic is connected), a file
browser for the default branch or a project's checkout (and
for a PR's or branch's Browse tab) that searches files by name, text, or
symbol, and
windows that are native tabs on macOS, each showing
its own of these. The "Not
built yet" section of `README.md` tracks the near-term gaps; the goals above
are the longer arc.

## Layout

```
.github/workflows/
  release.yml        Publishes main-latest on push to main, and stable builds to releases
  certificate-expiry.yml Opens an issue when the signing certificate is about to expire
assets/
  icon/              App icon SVG sources and rendered PNGs/.icns (see Icon)
  icons/             Lucide UI icons (ISC, see LICENSE there)
scripts/
  bundle.sh          Builds Docket.app with the docket:// URL scheme
  certificate-expiry.sh When a .p12's certificate expires, and whether that's soon
  package.sh         Builds Docket.app for a channel, zipped and as a disk image, for a release
  publish.sh         Uploads them to a channel's GitHub release
  release.sh         package.sh then publish.sh, from this Mac
  render-icons.sh    Rasterizes the icon sources
src/
  main.rs            App bootstrap, window, actions, menus, keybindings
  config.rs          OAuth client ID, scopes, API URLs
  http_client.rs     GPUI's HTTP client (ureq), for remote images like avatars
  markdown.rs        GitHub-flavored Markdown parsed into blocks for rendering
  ai/
    client.rs        Blocking Messages API client (API key or the Anthropic CLI's OAuth token)
    cli.rs           Token from the Anthropic CLI's login (`ant auth print-credentials`)
    store.rs         Keychain persistence of the Anthropic connection
    analysis.rs      A diff's hunks written out for Claude; its groups checked and mapped back
    cache.rs         Analyses on disk, by a hash of what went into them
    fake.rs          Canned Anthropic answers for tests
  test_support.rs    UI tests: Docket in a headless window, input helpers, fixtures
  updater.rs         Downloads, checks, and installs a release over the running bundle
  lsp/
    client.rs        JSON-RPC over a language server's stdio: hover, symbols, definitions, progress
    servers.rs       Known servers, finding each from the project or system
    worktree.rs      Docket's worktree at the PR's head, with the checkout's dependencies
    symbols.rs       A file's outline, and the scopes a line is in
  state/
    mod.rs           AppState entity: auth status, PR sections, projects, branches, async tasks
    tab.rs           A window's Tab: selected project, what's open
    docket.rs        The docket: PRs and branches, grouped and ordered; loads branches
    live.rs          Polling that keeps the docket and the open PR up to date
    projects.rs      Saved projects, and the add/edit setup flow
    pr_detail.rs     The open pull request and its expanded/collapsed entries
    review.rs        The open diff (a PR's Files tab, or a branch's): loading and highlighting it
    analysis.rs      The Anthropic connection, and grouping the open diff's changes
    branch.rs        The open branch: its diff and commits since it left the default branch
    code_intel.rs    Language servers for the open diff and browser, request targets, outlines
    browse.rs        The browsed files (default branch, checkout, or Browse tab): tree, open file or directory
    search.rs        Searching the browsed files by name, text, or LSP symbol
    updates.rs       Checking the channel's release hourly, and the update's status
    viewed.rs        Viewed files, shared by every window: GitHub's for PRs, saved ones otherwise
  auth/
    device_flow.rs   Device-code request and token polling
    github_cli.rs    Token from the GitHub CLI's login
    token_store.rs   Keychain (or, in debug builds, file) persistence of the sign-in
  diff/
    mod.rs           Diff model: files with both sides' full text
    compute.rs       Whole-file line alignment and visible excerpts
    git.rs           Loads a PR's diff from the local checkout
    text.rs          One side of a file, indexed by line
    viewed.rs        Files marked as viewed, keyed by what the diff shows of them; on disk
  syntax/
    mod.rs           Highlight names and per-file syntax snapshots
    languages.rs     Bundled tree-sitter grammars, detected by path
    highlight.rs     Parses and flattens highlight captures into spans
  github/
    client.rs        Blocking GraphQL client (run on the background executor)
    queries.rs       GraphQL documents and wire types
    models.rs        Domain types used by the UI
    links.rs         github.com links Docket opens itself (pull requests)
    fake.rs          Canned GitHub answers for tests
  projects/
    branches.rs      A checkout's default branch (and base), your unmerged branches, their commits
    files.rs         Lists a checkout's (or a commit's) directories as Git would, and reads files
    search.rs        Every file in a tree; fuzzy name matching and ripgrep-style text search
    git.rs           Reads a checkout's root and GitHub remotes
    store.rs         projects.json persistence
  ui/
    windows.rs       Opening windows and native tabs
    workspace.rs     A window's root view; switches between sign-in and dashboard
    sign_in.rs       Sign-in screen (device flow, GitHub CLI, token)
    text_input.rs    Single-line text field (adapted from GPUI's example)
    sidebar.rs       User and projects
    sidebar_stack.rs Sidebars per level of what's open; one expanded, the rest rails
    pr_list.rs       Project header + the docket; its sidebar form
    pr_detail.rs     A pull request's conversation, merge status, reviewers
    branch_view.rs   A branch's header, title, and tabs, above its diff
    code/            Shared by views of code: selection, copying, hover, definitions
    commit_list.rs   A PR's or branch's Commits tab, grouped by day
    diff_view/       A PR's Files tab or a branch's diff: file list, rows, hunk-header scopes, groups
    file_browser/    Browsed files: directory tree and its search, highlighted file and find, or directory listing
    project_setup.rs Connect a folder to its GitHub repository
    theme.rs         Semantic color tokens, syntax colors, light and dark palettes
    components/
      icon.rs        Lucide icons, asset source, spinner
      button.rs      Button, IconButton, tooltip
      list.rs        ListItem, badges, section headings, callouts
      tabs.rs        TabBar: a view's tabs, like a PR's Conversation and Files
      tree.rs        TreeItem: a row of the file browser's tree or a diff's file list
      markdown.rs    Renders a parsed Markdown document
      nested_scroll.rs Lists that scroll inside a scrolling page, without moving it
```

## Conventions

- **State** that every window shares (sign-in, PRs, projects, branches) lives in
  the `AppState` entity, registered as a GPUI global so actions can reach
  it. What a window shows lives in its `Tab` entity (`state/tab.rs`), which
  observes `AppState` and follows its `AppEvent`s. Views take the `Tab`,
  observe it, and re-render on `cx.notify()`. Actions on what a window shows
  are handled on its `Workspace`'s root element; app-wide ones (New Tab,
  Quit) are global.
- **Network calls** are blocking (`ureq`) and always run via
  `cx.background_executor().spawn(...)`. Results are applied back with
  `WeakEntity::update`. In-flight `Task`s are stored on the entity so they are
  cancelled when replaced or dropped.
- **GitHub data** comes from GraphQL. Add a query in `github/queries.rs`, map
  it to a type in `github/models.rs`, and expose a method on `GithubClient`.

## Design system

Views are built from `ui/components` and styled with the tokens in
`ui/theme.rs`; don't use raw colors in views.

- **Theme.** `Theme` is a GPUI global holding semantic tokens: surfaces
  (`background`, `surface`, `elevated`), translucent `element_hover` and
  `element_selected` overlays, borders, text levels (`text`, `text_muted`,
  `text_subtle`), accent, and status colors (`success`, `warning`, `danger`,
  `merged`) each with a `*_subtle` fill. Diffs add `diff_*` line fills and
  `syntax`, a style per highlight name. It follows the system appearance;
  View → Appearance overrides it for the session.
- **Color.** Components take a semantic `Color` (`Muted`, `Danger`, …) rather
  than an `Hsla`, so they resolve against whichever palette is active.
- **Icons.** `Icon::new(IconName::GitMerge)` with an `IconSize` (12/14/16/20px)
  and a `Color`. Icons are [Lucide](https://lucide.dev) SVGs embedded at
  compile time. To add one, copy it from the `lucide-static` package into
  `assets/icons/` and add a line to the `icons!` list in
  `components/icon.rs`.
- **Components.** `Button` (primary, secondary, ghost, danger; optional
  leading icon), `IconButton` (always with a tooltip), `ListItem` for
  navigation rows, `TreeItem` for rows of a file tree (indented, with a
  disclosure chevron for directories), `TabBar` and `TabBarItem` for a
  view's tabs (a PR's and a branch's), `Badge` for status pills, `Callout` for errors and warnings,
  `Spinner` for loading, and `TextInput` (`ui/text_input.rs`) for single-line
  fields.

## Testing

`cargo test` runs everything. Check UI changes with a UI test rather than by
launching the app.

UI tests run Docket headlessly on GPUI's test platform (`#[gpui::test]`):
the window is never shown and input goes straight to it, so they don't
move the mouse, take focus, or need a display. `test_support::open_docket`
opens a window signed in to a `FakeGithub` seeded with the test's pull
requests and projects, and returns its `Tab`; `docket_windows` and
`window_context` reach windows opened since, like new tabs. Tests click elements by test selector
(`cx.click_on("project-all")`), type keys (`cx.simulate_keystrokes("down
right")`), and check what's drawn (`cx.is_drawn(&pr.id)`) or the state
behind it (`state.read_with(cx, …)`). See `ui/workspace.rs` and
`ui/file_browser` for examples.

Components register their id as a test selector (`ListItem`, `Button`,
`IconButton`), as do PR rows (the PR's `id`). Give anything else a test
needs to find one with `.test_selector(|| …)`; it costs nothing outside
tests. GPUI's test platform measures text with placeholder metrics and
paints nothing, so tests check behavior and structure, not appearance.

Under `cfg(test)` nothing reaches the user's sign-in, projects, or GitHub:
the token store keeps nothing, `projects.json` isn't read or written, the
GitHub CLI and device flow refuse, and only a faked `GithubClient` answers.
Tests that need a checkout make one in a temp dir with
`test_support::git_checkout`. Avoid opening files in a language with a
server (Rust, TypeScript, Go, Python) in tests, since that starts the real
language server.

## Implementation notes

### Sidebars

The window's left edge is a stack of sidebars, one per level of what's
open: navigation; then the docket, once a pull request or branch is open
from it; then the changed files of the pull request (on its Files tab) or
branch, the tree on their Browse tab, or the default branch's tree. Only the deepest is expanded, and the rest
collapse to rails of icons that act like their rows. Resting the mouse on a
rail for a moment (`PEEK_DELAY`) expands it in the deepest's place; only
resting changes which is expanded, so the mouse can cross from one sidebar
to the next, and it closes when the mouse leaves the stack. The lines
between sidebars belong to neither. Every sidebar is `SIDEBAR_WIDTH` expanded and
`RAIL_WIDTH` collapsed, so swapping which is expanded never moves the view
beside them. To add a level, add a `SidebarLevel`, push it in
`Workspace::sidebar_levels`, and render it both ways in
`Workspace::sidebar_content`. A sidebar whose rows belong to a view (the
file tree's virtualized list) is built by that view (`tree_sidebar`) and
placed in the stack by the workspace.

### Docket

`AppState::docket` merges the three PR searches (review requested,
authored, involved) with each project's branches. Each project's default
branch is pinned first; the rest is grouped by status (`docket::Group`):
review requested, your approved PRs (ready to merge), your other PRs
waiting on review, your drafts, others' PRs
you're involved in, then branches without a PR, each by last update.
Headings show only with more than one group. Branches show back as many weeks
as the tab's `weeks` (one, plus one per Show more); PRs always show. The cutoff counts back from
`docket::now()`, which tests fix at 2026-01-07.
`projects/branches.rs` finds branches with one `git for-each-ref
--no-merged=<default>` and one `git log` over the unmerged commits, looking
for `user.email`. A branch is hidden once a PR is from it (by head ref
name or tip). Clicking the default branch browses its files; any other
branch opens its diff (`state/branch.rs`). The default branch's row has a
button that pulls it from the project's remote (`branches::pull`): `git
pull --rebase --autostash` where it's checked out, aborting a rebase that
conflicts, and a fetch and fast-forward where it isn't.

### Pull request detail

The detail is one GraphQL query plus up to nine more pages of timeline
(1,000 items). Bodies are parsed as Markdown on the background executor;
HTML is reduced to its text and HTML comments are dropped.

Links dispatch `OpenLink`, which the window's `Workspace` handles:
`github::PullRequestLink` picks out pull requests (and which tab), which
open in that tab, and the rest go to the browser. Links from other apps
(`docket://` and github.com ones, from GPUI's `on_open_urls`) reach the
app over a channel, since its callback has no `App`, and
`windows::open_links` opens them in the front window; it drops the rest so
a link router can't bounce them back. A tab holds a link followed before
sign-in (`pending_link`) until `AppEvent::SignedIn`. A pull request opened
from a link uses the docket's row when it has one. Otherwise
`PrDetail::summary` is `None` until the detail loads (its query fetches
the row's fields too), and its diff waits for the commits.

### Live updates

GitHub can't push to a client without a backend (webhooks need a server,
and GraphQL has no subscriptions), so `state/live.rs` polls. The docket's
searches (a point each) load again every `DOCKET_INTERVAL`. The open pull
request is watched by its version (`PULL_REQUEST_VERSION`, one point, which
changes whenever anything the detail shows does) every
`PULL_REQUEST_INTERVAL` while its window is active, and its detail (dozens
of points) is fetched only when that changes. Polled loads are quiet: no
spinners, and what's shown stays. Each loop sleeps until its next check is
due, so a refresh or a window becoming active (`Tab::set_window_active`,
which catches up at once) pushes the next poll back. Tests drive polling
with `cx.executor().advance_clock(…)`; `FakeGithub::set_detail` bumps the
fake's version.

### Diffs

Loading a diff (`diff/git.rs`):

1. If the PR's head or base commit isn't local, fetch it into
   `refs/docket/pull/<n>/head` and `refs/docket/pull/<n>/base`. Git runs with
   `GIT_TERMINAL_PROMPT=0`, so it fails instead of waiting on a credential
   prompt.
2. Diff the merge base against the head, detecting renames.
3. Read both sides of each file with `git cat-file --batch`.

A branch's diff (`load_branch`) is steps 2 and 3, from its merge base with
the default branch (`Branches::base`, the remote's where there is one) to
its tip; both are already local. Its Commits tab is a `git log` of the same
range (`branches::commits`); a PR's comes from its timeline. The open diff, a PR's or a branch's, is a
`DiffFiles` (`state/review.rs`), which the diff view, its file list, and
language servers read through `Tab::diff_files`.

A PR's diff starts loading when the PR opens, not when its Files tab
does: in a large repository the fetch alone takes seconds, so it runs while
the conversation is read. Language servers wait until the diff is on screen
and highlighted, since preparing their worktree and indexing would compete
with it.

Files are in the changed-files tree's order (`diff::sort_files`), the
file browser's: in each directory its subdirectories, then its files,
compared a component at a time, with top-level files last, so everything
under a directory is contiguous. The tree (`diff_view/file_list.rs`) nests
directories, compacting a chain that holds nothing but one another into one
row (`3rdparty/python`); collapsing one only hides what's under it there,
and the diff still shows it. The tree is a GPUI `list` rather than a
uniform one, since long group titles wrap; `FileTree::update` splices only the
rows that changed, so collapsing keeps its place.

A file marked as viewed collapses until it changes. Each `FileDiff` has a
`ViewKey`, a hash of its paths, status, and the lines its excerpts show
(both blobs' OIDs when its text isn't shown), computed in `diff/git.rs`;
a rebase that changes only other lines keeps it. `ViewedStore`
(`diff/viewed.rs`) keeps, per `DiffFiles::viewed_scope` (`owner/name#1`
for a pull request, `owner/name@branch` for a branch) and path, the key
each file had when it was marked; a different key now is
`Viewed::Changed`. It's on `AppState`, so windows agree, and saved to
`viewed.json` in the data directory (not under `cfg(test)`). The
`DiffView` shows a file collapsed when it's viewed, unless `toggled`: a
file's header flips it from that default, recorded with its key, so the
flip lapses when the file changes. Marking or unmarking clears the flip.

A pull request's viewed files are GitHub's (`state/viewed.rs`):
`AppState::viewed` answers from its `viewerViewedState`s once they've
arrived, and from the store until then. They come with
`PULL_REQUEST_VERSION`, as `viewerViewedFiles`, which
`GithubClient::pull_request_version` takes out of the version so a mark
doesn't fetch the detail; with more than a page of files, `viewed_files`
fetches them all. Marks go out with `markFileAsViewed` and
`unmarkFileAsViewed`, chained so they land in order. Until GitHub confirms
one it's `unsynced` and shows in place of GitHub's state; one GitHub
refuses stays there, with `viewed_error` and Try Again
(`retry_viewed`), until it's sent or GitHub agrees. GitHub's states are
saved to the store as they arrive. Tests set GitHub's side with
`FakeGithub::set_viewed_state`, and `fail_viewed` and `set_many_files`.

`diff/` aligns every line of the old and new file once; excerpts (changes
with three lines of context) choose what's visible. Having both sides in
full is what expanding context, editor integration, and LSP positions build
on.

### Change analysis

With Anthropic connected (`AppState::anthropic`), a diff shows grouped by
change: `claude-haiku-5-5` sorts its hunks into logical groups, each with a
title, summary, and impact. `AnalysisRequest` (`ai/analysis.rs`) is built
on the background executor with the diff (`load_diff`), writing every
excerpt (or a whole binary or unchanged file) as `H1`, `H2`, … in the
diff's order; one Messages request with structured outputs
(`output_config.format`) answers with groups of IDs, and `resolve` drops
unknown and repeated ones and gathers any left out into "Other changes", so
every hunk shows once. Analysis starts once the diff loads
(`Tab::analyze_diff`), and again on `AppEvent::AnalysisEnabled` (connecting,
or turning grouping on). Its key hashes the model, prompt version, system
prompt, and the written-out diff, so it changes exactly when the diff does;
analyses are kept by it in `AppState::analyses` and on disk
(`ai/cache.rs`). Bump `analysis::VERSION` when the prompt or the shape of
an analysis changes.

Grouped, the diff (`build_grouped_rows`) heads each group's hunks with its
title, then its summary and impact: rows can't grow, so they're wrapped at
`WRAP_COLUMNS` into `GroupText` rows. The changed-files tree has a row per
group, its title, with a tree of its files under it (`grouped_tree_rows`).
A file with hunks in several groups is under each; `file_headers` points
at its first header, and `group_file_headers` at each, which a file under a
group in the tree scrolls to. Directories collapse per group (`DirKey`).

Anthropic has no OAuth apps for other clients, so its OAuth is the
Anthropic CLI's: `ai/cli.rs` runs `ant auth print-credentials
--access-token` per request (it refreshes), sent as a bearer token with
`anthropic-beta: oauth-2025-04-20`. Otherwise an API key, in the keychain
(`auth::SecretStore`). Tests connect a `FakeAnthropic`
(`AppState::connect_anthropic_for_test`); `AnthropicClient` refuses to reach
the network under `cfg(test)`, and nothing is cached on disk.

### Syntax highlighting

Highlighting runs per file on the background executor with
[tree-sitter](https://tree-sitter.github.io), and lines render plain until it
finishes. Each file keeps its syntax tree for structural features. To add a
language, add its grammar crate and an entry in `syntax/languages.rs`.
Highlight names map to `Theme::syntax` by longest prefix (`function.method`
falls back to `function`).

### File browser

Each directory is listed from disk the first time it's expanded, so opening
a large checkout doesn't walk all of it. Docket reads the index once (`git
ls-files --cached`, which needs no walk). Each directory is then a
`read_dir`, with its untracked entries checked against Git's ignore rules by
the [`ignore`](https://docs.rs/ignore) crate (ripgrep's and fd's), limited to
Git's own sources: the `.gitignore`s from the repository's root down,
`.git/info/exclude`, and the global excludes file. Its other filters (hidden
files, `.ignore`) are off. In a checkout of about 200,000 files, the tree
opens in about 0.15 s and a directory lists in a few milliseconds.

Every browser is on disk. A checked-out branch (`%(worktreepath)`) is
browsed in its worktree; any other (`BrowseSource::DocketWorktree`) is
checked out at its tip in Docket's worktree for the project
(`lsp::worktree::prepare`, under `.git` in tests) before it's listed, so
hover, definitions, and symbols never depend on where a branch is.

Files load and highlight on the background executor with the same limits as
diffs. The open file's code view is `ui/code`, shared with the diff.

A PR's or branch's Browse tab is the same `ProjectBrowser` in `Tab::browser`,
beside `detail` or `branch` rather than in its place; its `BrowserScope`
says which, and it closes with them. `Tab::sync_tab_browser` opens it the
first time the tab shows, and moves it when the head does, keeping what's
expanded and open. A PR's checks out its head in Docket's worktree once
its diff has fetched it; a branch's is where it's checked out, else its tip
in Docket's worktree. Either way `FileBrowserView` draws no header of its
own there. In Docket's worktree the tab is at the diff's head, where the
diff's servers run, so it uses those (`Tab::browser_intel_key`): files it
opens add servers to the diff's `CodeIntel` (`ensure_diff_server`, trusted
as the diff is), and the Files tab adds the diff's languages to those it
started (`add_diff_servers`). Read the browser's servers through
`Tab::browser_code_intel` and `browser_lsp_target`, not
`ProjectBrowser::code_intel`.

While no file is open, the main panel lists `ProjectBrowser::dir`, which is
listed along with the expanded directories (`listed_paths`). Navigating
from it expands the tree to match. The tree's arrow keys only open files;
a click or Enter on a directory lists it.

### File search

The tree's sidebar searches its files by name, text, or symbol
(`state/search.rs`, `ui/file_browser/search.rs`), with one query for all
three. Names and text search a `FileIndex` (`projects/search.rs`): every
file the tree lists, from a parallel `ignore` walk (fd's approach) plus
tracked files in ignored directories. It's
built on first use and dropped with each listing. Names match with
`nucleo-matcher` (Helix's fzf algorithm), in parallel past 20,000 paths.
Text is searched with ripgrep's crates (`grep-searcher`, `grep-regex`),
smart case, on a thread per core. Opening files dominates a search on
macOS, so tracked files unchanged since the index (`git ls-files -s` less
`git diff-files`, kept for five seconds) are read through `git cat-file
--batch`, one per thread; only changed and untracked files are opened. In a
216,000-file checkout, a name search is about 2 ms and a full text search
about 0.6 s, against ripgrep's 3.7 s. Symbols come from `workspace/symbol`
on each running server for the index's languages (choosing the mode starts
them), filtered to the checkout and ranked by name; `update_server`
searches again when a browser server starts or finishes loading. A new
query drops the search in flight, and a `Cancel` flag stops text-search
threads. Text and symbols wait `SEARCH_DEBOUNCE` first; tests advance the
clock past it. Escape is the app's `Cancel`, and a binding without a
context outranks every context's, so the search bar handles `Cancel`
itself rather than binding `escape`. `DOCKET_SEARCH_BENCH=<checkout> cargo
test --release -- --ignored search_benchmark --nocapture` times one.

Find in the open file (`ui/file_browser/find.rs`) is the view's own
state, like its selection: `find_in_text` matches its `TextSnapshot` line
by line with the same matcher as text search, on the background executor,
whenever `sync_find` sees the query or the text change. Matches reach the
lines through `Marks::found`, drawn as `find_match` backgrounds
(`find_match_current` for the one it's on). The bar is inside the view,
whose `FileBrowser` context binds the arrows to the tree, so its
`FileFind` context binds them to the next and previous match; escape
closes it through `Cancel`, and the view lets `Cancel` through when it's
closed. Its Symbols mode (`outline.rs`, ⌘⇧O) lists the file's
`documentSymbol` outline, asked of `ProjectBrowser::lsp_target` once the
server runs and again when the text or server changes, in a `deferred`
list under the bar; the selected name is the only `Marks::found`. Tests
can't run a server, so they set the outline (`set_outline`).

### Updates

Docket updates from GitHub releases of `RELEASES_OWNER`/`RELEASES_NAME`
(`config.rs`), with the user's sign-in, since the repository may be
private. Each build follows a `updater::Channel`, baked in at build time
along with its commit (`DOCKET_CHANNEL` and `DOCKET_COMMIT`, which
`scripts/bundle.sh` sets and also writes to the bundle's Info.plist as
`DocketChannel` and `DocketCommit`). Stable follows the latest release
(`GithubClient::latest_release`), tagged `v<version>`, and updates to a
higher version. Main follows the `MAIN_TAG` prerelease (`release`), which
`scripts/publish.sh --channel main` recreates at `main`'s tip, and updates
to any other commit. `.github/workflows/release.yml` publishes it on every
push to `main`, and a published release's stable builds. It builds on an
arm64 and an Intel macOS runner (`package.sh`), then publishes both builds
from one job (`publish.sh`), so `main-latest` is replaced once. Builds
are signed with `DOCKET_SIGN_IDENTITY` (hardened runtime, timestamp) or
else ad hoc, and notarized by `package.sh` given `DOCKET_NOTARY_KEY`; the
workflow sets these from secrets when they're there: the repository's,
or those of the environment `vars.DOCKET_RELEASE_ENVIRONMENT` names.
`certificate-expiry.yml` reads the same certificate weekly
(`scripts/certificate-expiry.sh`, which retries a `.p12` with `-legacy`
for Keychain Access's RC2): within 60 days of expiry it opens a
`certificate-renewal` issue with the steps to renew it, failing once it
has expired, and closes the issue once the certificate is renewed.
Renewal stays manual on purpose: automating it would take an Admin App
Store Connect key and a token that writes secrets, both in CI, and Apple
lets only the Account Holder create Developer ID certificates. CI
precompiles shaders (`--no-default-features`) on macOS 26 runners with
Xcode 26.6 pinned (`DEVELOPER_DIR`), downloading its Metal Toolchain,
which Xcode 26 doesn't include.
`Update::from_release` decides; an `Update` is a version or a commit.
Releases attach `Docket-macos-<arch>.zip` (`std::env::consts::ARCH`), a
`ditto` zip of the bundle, which updates download, and
`Docket-macos-<arch>.dmg`, for people to install from, which they don't.
With a notary key, `package.sh` notarizes each: the zip, then, with the
ticket stapled in the bundle, the disk image. Both queries are
GraphQL: each asset's `url` is a download link signed for about an hour
that needs no token, so `download_release_asset` sends none.
`state/updates.rs` checks every `UPDATE_INTERVAL` once signed in, only
when running from a bundle (`updater::running_bundle`, never in tests). An
update installs at once (`updater::install`): unpacked in
`.docket-update` beside the bundle, so it's on the same volume, checked
(size, `digest`, bundle ID, channel, version or commit, `codesign
--verify`, and, unless the running bundle is signed ad hoc, its designated
requirement via `--test-requirement`, so nothing moves installs to another
signer: changing it means installing by hand; and for an update signed with a
Developer ID, `spctl --assess`, so revoked certificates and notarizations
don't install), then renamed into place, the old bundle
moved back if that fails. Restart to Update (`updater::relaunch`) waits for
the process to exit and `open`s the bundle. Automatic checks show nothing
until there's an update; Check for Updates… shows checking, up to date,
and errors too. Tests set a bundle in a temp dir (`set_update_bundle`), and
a channel (`set_update_channel`), and publish fake releases
(`FakeGithub::set_latest_release`, `set_release`) of fake builds
(`updater::tests::fake_bundle`).

### Windows and tabs

Each window is a `Workspace` with its own `Tab`, so tabs are independent,
language servers included. On macOS they share a tabbing identifier, which
gives them AppKit's tab bar and Window menu commands. GPUI adds a new window
to the tab group only when the user's "Prefer tabs" setting says to, so New
Tab and opening a definition in a new tab call `addTabbedWindow:ordered:`
through `objc2-app-kit` (`ui/windows.rs`), then tell GPUI's
`SystemWindowTabController`, which tracks groups for Show Next Tab. GPUI's
window class also hides AppKit's tab bar (Zed draws its own), so
`windows::init` points its `addTitlebarAccessoryViewController:` override
back at `NSWindow`'s at launch. It also adds `newWindowForTab:`, which
AppKit's "+" button needs; that sends the clicked window over a channel to
a task on the app, which opens the tab. Test
windows aren't real, so there tabs are just windows. A window's title,
which labels its tab, is `Tab::title`.

Opening a window from an action has to wait for the action to finish
(`cx.defer`): while it's dispatched, its window can't be read or updated.

### Language servers

Requests used: `textDocument/hover` for hover, `textDocument/documentSymbol`
for hunk-header scopes, `textDocument/definition` for go to definition, and
`workspace/symbol` for the file browser's symbol search.
To support another server, add an entry to `SERVERS` in `lsp/servers.rs`.

## Icon

The icon is a "D" drawn as a git graph: the stem is the base branch, the bowl
is a pull request branching off (blue, open) and merging back (purple,
merged), and the amber node is the PR pending on you. The SVG sources in
`assets/icon/` are canonical:

| Source                 | Use                                                        |
| ---------------------- | ---------------------------------------------------------- |
| `docket.svg`           | App icon on the macOS grid (64px and up)                   |
| `docket-small.svg`     | Simplified app icon for 16 and 32px                        |
| `docket-square.svg`    | Full-bleed square: GitHub OAuth App logo, Windows/Linux    |
| `docket-mark.svg`      | Monochrome glyph for in-app use (GPUI `svg()` tints it)    |
| `trayTemplate.svg`     | macOS menu bar template image, PRs pending                 |
| `trayIdleTemplate.svg` | macOS menu bar template image, nothing pending             |

`scripts/render-icons.sh` regenerates `assets/icon/docket.icns` and
`assets/icon/png/` (requires Google Chrome, or set `CHROME`). Commit the
rendered files alongside SVG changes. For the OAuth App logo, upload
`png/docket-square-512.png` with background color `#0d1117`.
