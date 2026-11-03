# Docket

A native desktop app for managing GitHub pull request workflows, built with
[GPUI](https://gpui.rs). There is no backend: the app signs in with GitHub
OAuth and calls the GitHub API directly.

## Setup

```bash
cargo run
```

The app ships with the project's OAuth App client ID (`src/config.rs`). To use
your own OAuth App instead:

1. Create one at <https://github.com/settings/developers>.
   - Callback URL: `http://127.0.0.1/callback` (unused by device flow, but required).
   - Check **Enable Device Flow**.
2. Set its client ID (no client secret is needed), either at runtime or at
   build time:

   ```bash
   DOCKET_CLIENT_ID=Ov23li... cargo run
   ```

### Metal shaders (macOS)

The default `runtime_shaders` feature compiles GPUI's Metal shaders at runtime,
so you don't need Xcode's Metal Toolchain. To precompile them (recommended for
release builds):

```bash
xcodebuild -downloadComponent MetalToolchain
cargo build --release --no-default-features
```

## Authentication

Uses the [OAuth device flow](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow),
which is designed for clients that can't keep a secret:

1. The app requests a device code and shows the user a short code.
2. The user enters it at github.com/login/device.
3. The app polls until GitHub issues an access token.

The token is stored in the OS keychain (service `dev.docket`) and restored
on launch. Requested scopes: `repo`, `read:org` (see `src/config.rs`).

Debug builds store it in `development_credentials` under the platform config
dir instead (readable only by you). macOS grants keychain access by code
signature, and every `cargo build` re-signs the binary, so the keychain would
prompt after each rebuild. Set `DOCKET_USE_KEYCHAIN=1` to use the keychain in a
debug build.

### Orgs that restrict OAuth apps

Organizations can block OAuth apps they haven't approved, and every OAuth
token is tied to the app that requested it, so device flow can't reach those
orgs' repositories until an owner approves Docket. The sign-in screen offers
two ways to sign in as yourself instead:

- **GitHub CLI login.** Uses `gh auth token`, so it works wherever the GitHub
  CLI is approved. Docket stores only the choice and re-reads the token from
  `gh` on each launch, so `gh auth refresh`/`logout` carry over. `gh` is found
  on `PATH` or in `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`.
- **Personal access token.** A classic token (recommended) needs the `repo`
  and `read:org` scopes. A fine-grained token covers a single resource owner,
  so choose the org whose PRs you want, give it all of that owner's
  repositories, and grant read-only **Pull requests**, **Contents**, **Commit
  statuses**, and **Issues** (repository) and **Members** (organization).
  Fine-grained tokens can't read check runs, so GitHub Actions results won't
  show. Either may need SSO authorization, or org approval for fine-grained
  tokens, depending on the org's policies. Stored in the keychain once GitHub
  accepts it. The sign-in screen lists all of this next to the token field.

GitHub search silently leaves out repositories a sign-in can't reach, such as
an org whose SSO the token isn't authorized for, so an inbox can look empty
when it isn't. On each refresh Docket checks every project's repository and,
where GitHub hides it, flags the project and explains why above the PR list,
with a link to fix it. Orgs with no project in Docket aren't checked.

## Projects

A project is a local Git checkout connected to the GitHub repository its PRs
live in. Selecting one in the sidebar scopes the inboxes to that repository;
the local path is what editor integration and LSP will build on.

Adding a project (`cmd-o`) asks for a folder, resolves it to the checkout
root, reads its GitHub remotes, and checks each on GitHub. It pre-selects the
most likely target: an `upstream` remote or a fork's parent over `origin`.

Projects are stored locally in `projects.json` under the platform config dir
(`~/Library/Application Support/dev.docket/` on macOS), independent of the
signed-in account.

### Browsing files

**Browse Files** in a project's header shows its checkout as it is on disk,
uncommitted changes and untracked files included: a directory tree beside the
selected file, highlighted as in the Files tab. Its header names the
checked-out branch.

Each directory is listed from disk the first time it's expanded, so opening
a large checkout doesn't walk all of it. A directory shows what `git ls-files
--cached --others --exclude-standard` would: tracked entries still on disk,
plus untracked ones Git doesn't ignore. So `target/` and `node_modules/`
stay out, but a tracked file in an ignored directory doesn't, and an
untracked folder holding only ignored files (a cache that ignores itself)
doesn't show either. Submodules and nested repositories show as one entry.

Docket reads the index once (`git ls-files --cached`, which needs no walk).
Each directory is then a `read_dir`, with its untracked entries checked
against Git's ignore rules by the [`ignore`](https://docs.rs/ignore) crate
(ripgrep's and fd's): the `.gitignore`s from the repository's root down,
`.git/info/exclude`, and the global excludes file named in `~/.gitconfig`
(or Git's default). Nothing else those tools skip, like hidden files or
`.ignore`, is skipped. In a checkout of about 200,000 files, the tree opens
in about 0.15 s and a directory lists in a few milliseconds.

The tree is keyboard-driven: `up`/`down` move through it and show each file
they reach, `right` expands a directory (or moves into an expanded one),
`left` collapses it (or moves to the one it's in), and `enter` toggles one.
Files load and highlight on the background executor with the same limits as
diffs: over 5 MB or binary shows a notice, and over 1 MB or 20,000 lines
shows plain text. `cmd-r` re-reads the index, the expanded directories, and
the open file.

The open file's code selects, copies, hovers, and jumps to definitions just
as in the diff (see Files, Hover, and Go to definition below; `ui/code` is
shared by both). Its language servers run
in the checkout itself, since that's the version being read, rather than in
Docket's worktree. Each starts the first time a file in its language opens,
and the status of each shows below the tree. A checkout is your own working
copy, as your editor would see it, so every server starts without asking,
including those that run project code. They stop when the browser closes.
rust-analyzer builds in your checkout's `target/`, so it can wait on a cargo
lock while your editor or a build holds one. When a refresh finds a file
changed, its server gets the new text.

## Pull request detail

Clicking a PR opens its conversation, loosely after GitHub's: the
description, the timeline, and (while open) where it stands on reviews,
checks, and merging, with reviewers, assignees, and labels alongside.

The timeline is trimmed toward signal:

- Runs of bookkeeping events (review requests, deployments, labels,
  assignments, force-pushes, …) collapse into one expandable row. Merges,
  closes, reopens, and draft changes stay visible.
- Consecutive commits collapse into one row.
- Bot comments and hidden comments collapse to a one-line preview. Bot
  *reviews* stay expanded: they're feedback on the code.
- Resolved and outdated review threads start collapsed.

The detail is one GraphQL query plus up to nine more pages of timeline
(1,000 items). Bodies are parsed as Markdown on the background executor;
HTML is reduced to its text and HTML comments are dropped.

### Files

The Files tab shows the PR's diff, loaded the first time the tab opens so
the conversation never waits on a fetch. Diffs come from the project's local
checkout, not GitHub's patch API, so Docket has both sides of every file in
full: syntax highlighting sees real context, and expanding context, editor
integration, and LSP have real positions to build on. A PR in a repository
with no project offers to add one.

Loading a diff:

1. If the PR's head or base commit isn't local, fetch it from the remote that
   points at the project's repository (or its GitHub URL) into
   `refs/docket/pull/<n>/head` and `refs/docket/pull/<n>/base`. That namespace
   leaves your branches and `FETCH_HEAD` alone. Git runs with
   `GIT_TERMINAL_PROMPT=0`, so it fails instead of waiting on a credential
   prompt.
2. Diff the merge base against the head, as GitHub does, detecting renames.
3. Read both sides of each file with `git cat-file --batch`. Files over 5 MB
   and binary files show a notice instead.

`diff/` aligns every line of the old and new file once; excerpts (changes
with three lines of context) choose what's visible. Highlighting runs per file
on the background executor with [tree-sitter](https://tree-sitter.github.io),
and lines render plain until it finishes. Each file keeps its syntax tree for
structural features. Languages: Rust, TypeScript/TSX, JavaScript/JSX, Python,
Go, JSON, TOML, YAML, shell, CSS, HTML, and Markdown. To add one, add its
grammar crate and an entry in `syntax/languages.rs`. Highlight names map to
`Theme::syntax` by longest prefix (`function.method` falls back to
`function`).

Code in the diff selects like text: drag, shift-click to extend, double-click
for a word, triple-click for a line, `cmd-a` for every line. `cmd-c` copies the
file's own text (tabs, not the spaces they display as), one line per row.

### Hover

Resting the mouse on code on the new side (context and added lines) shows the
language server's hover in a popover. Servers run against a Docket-managed
worktree at the PR's head, so the diff's line numbers are the file's on disk:

1. Once the diff loads, Docket checks out the head in its worktree for the
   project, `<cache dir>/dev.docket/worktrees/<project id>/<folder name>`
   (`~/Library/Caches/…` on macOS). It's a detached `git worktree`, made once
   per project and reused, so switching PRs only rewrites what differs and
   build output carries over. Hooks and LFS downloads are skipped. `git
   worktree list` in your checkout shows it.
2. Docket starts a server for each language in the diff, preferring the
   project's own toolchain, found from your checkout (which has its
   dependencies installed) rather than the worktree:

   | Language   | Project first                                          | Then the system                  |
   | ---------- | ------------------------------------------------------ | -------------------------------- |
   | Rust       | `rustup which rust-analyzer` for the checkout's toolchain | `rust-analyzer`               |
   | TypeScript | `node_modules/.bin/typescript-language-server`, run with the checkout's `tsserver.js` | `typescript-language-server` |
   | Go         | `go tool gopls` when `go.mod` declares it              | `gopls`                          |
   | Python     | `.venv/bin/basedpyright-langserver` (or pyright, pylsp), with the venv's interpreter | the same on `PATH` |

   System servers are found on `PATH` or in the usual install directories
   (Homebrew, nix, mise, asdf, Volta, `~/.cargo/bin`, `~/go/bin`, …). For
   TypeScript, Docket passes the checkout's `tsserver.js`, following pnpm
   aliases such as `typescript: npm:@typescript/typescript6@…` to the real
   one, and the memory limit the project sets for VS Code
   (`typescript.tsserver.maxTsServerMemory` in `.vscode/settings.json`);
   large projects crash tsserver's default heap.
   rust-analyzer runs with `RUSTUP_TOOLCHAIN` set to the checkout's
   toolchain, so a toolchain file in the PR doesn't pick what runs. Docket
   doesn't install servers. To add one, add an entry to `SERVERS` in
   `lsp/servers.rs`.
3. The file list's footer shows each server and where it came from
   ("project TypeScript 6.0.2"). A server counts as loading from when it
   starts until it says it's done; a hover answered meanwhile is marked as
   possibly incomplete, since typescript-language-server answers from a
   partial view of the project (where imports are `any`) until it has
   loaded the rest. A server that isn't found shows the command that installs it; Check
   Again looks for it without reopening the PR. Servers stop when the PR
   closes or its diff reloads.

Some servers run code from the project: rust-analyzer runs build scripts,
proc macros, and commands from `.cargo/config.toml` or `rust-analyzer.toml`,
and Go runs the toolchain `go.mod` names. For a PR from a fork, that's code
from whoever opened it, so rust-analyzer and gopls wait for you to start them
from the footer, until the PR's head moves. PRs on a branch of the repository
(which takes write access to push) and your own PRs start them right away.
The TypeScript and Python servers only read code, and use your checkout's
TypeScript and virtualenv, not the PR's.

Git doesn't track installed packages or generated files, so the worktree
has neither, and TypeScript would type most of a project as `any`. Before a
TypeScript server starts, Docket symlinks them in from your checkout: each
package's `node_modules`, and the ignored declaration files your build
generated beside their sources (`*.module.css.d.ts`). They're your
checkout's versions, not the PR's, and Git ignores the links.

Removed lines have no hover: they don't exist at the head.

### Scopes in hunk headers

Hunk headers name the scopes their excerpt is in, like the function name
`git diff` puts after `@@`, but from the language server's outline of the
file (`textDocument/documentSymbol`) rather than a regex: `impl AppState ›
load_diff`. Only scopes that contain the excerpt's first change, and whose
names are above the excerpt, are shown. A scope whose start the excerpt
already shows isn't repeated. Each name hovers like code, showing the
server's hover for its declaration.

Outlines come from the same servers as hover, once each is running, for
files on the new side whose excerpts start partway in. Names appear a few
seconds after the diff. Deleted files, and anonymous scopes some servers
leave out of outlines (TypeScript's `export default { … }`), get none.

### Go to definition

Command-clicking code asks its language server where the symbol is defined
(`textDocument/definition`); so does `F12`, for the word at the start of the
selection. Holding command over code a running server can answer for
underlines it as a link. Wherever hover works, this does too, including the
scope names in hunk headers.

Where the definition shows depends on the view:

- **Diff.** On a line the diff shows (context or added, in any file of the
  PR), Docket scrolls to it and selects its name, expanding the file if it's
  collapsed.
- **File browser.** In the checkout, Docket opens the file, selects it in
  the tree with its directories expanded, and scrolls to the name.
- **Anywhere else** (in the diff, a file or line the PR doesn't change; in
  either, a file outside the project, like the standard library or a
  dependency in `~/.cargo/registry`), a peek popover shows the definition's
  code below the symbol: its path and line, and its lines, highlighted, from
  the start of its doc comment through the end of its body when the server
  says where those are (up to 80 lines), else a dozen lines from its name.
  Scrolling, clicking elsewhere, or `escape` closes it.

`ctrl--` goes back to where each jump came from, file and selection
included. With several definitions, Docket shows the first it can in place,
else peeks at the first. With none, the hover popover says so, or that the
server is still loading the project.

## Layout

```
assets/
  icon/              App icon SVG sources and rendered PNGs/.icns (see Icon)
  icons/             Lucide UI icons (ISC, see LICENSE there)
scripts/
  render-icons.sh    Rasterizes the icon sources
src/
  main.rs            App bootstrap, window, actions, menus, keybindings
  config.rs          OAuth client ID, scopes, API URLs
  http_client.rs     GPUI's HTTP client (ureq), for remote images like avatars
  markdown.rs        GitHub-flavored Markdown parsed into blocks for rendering
  test_support.rs    UI tests: Docket in a headless window, input helpers, fixtures
  lsp/
    client.rs        JSON-RPC over a language server's stdio: hover, symbols, definitions, progress
    servers.rs       Known servers, finding each from the project or system
    worktree.rs      Docket's worktree at the PR's head, with the checkout's dependencies
    symbols.rs       A file's outline, and the scopes a line is in
  state/
    mod.rs           AppState entity: auth status, PR sections, async tasks
    projects.rs      Project selection and the add/edit setup flow
    pr_detail.rs     The open pull request and its expanded/collapsed entries
    review.rs        The open PR's Files tab: loading and highlighting its diff
    code_intel.rs    Language servers for the open PR and browser, request targets, outlines
    browse.rs        The browsed project's file tree and open file
  auth/
    device_flow.rs   Device-code request and token polling
    github_cli.rs    Token from the GitHub CLI's login
    token_store.rs   Keychain (or, in debug builds, file) persistence of the sign-in
  diff/
    mod.rs           Diff model: files with both sides' full text
    compute.rs       Whole-file line alignment and visible excerpts
    git.rs           Loads a PR's diff from the local checkout
    text.rs          One side of a file, indexed by line
  syntax/
    mod.rs           Highlight names and per-file syntax snapshots
    languages.rs     Bundled tree-sitter grammars, detected by path
    highlight.rs     Parses and flattens highlight captures into spans
  github/
    client.rs        Blocking GraphQL client (run on the background executor)
    queries.rs       GraphQL documents and wire types
    models.rs        Domain types used by the UI
    fake.rs          Canned GitHub answers for tests
  projects/
    files.rs         Lists a checkout's directories as Git would, and reads files
    git.rs           Reads a checkout's root and GitHub remotes
    store.rs         projects.json persistence
  ui/
    workspace.rs     Root view; switches between sign-in and dashboard
    sign_in.rs       Sign-in screen (device flow, GitHub CLI, token)
    text_input.rs    Single-line text field (adapted from GPUI's example)
    sidebar.rs       User, projects, and inbox sections
    pr_list.rs       Project header + PRs for the selected section
    pr_detail.rs     A pull request's conversation, merge status, reviewers
    code/            Shared by views of code: selection, copying, hover, definitions
    diff_view/       The Files tab: file list, rows, hunk-header scopes
    file_browser/    A project's files: directory tree and highlighted file
    project_setup.rs Connect a folder to its GitHub repository
    theme.rs         Semantic color tokens, syntax colors, light and dark palettes
    components/
      icon.rs        Lucide icons, asset source, spinner
      button.rs      Button, IconButton, tooltip
      list.rs        ListItem, badges, section headings, callouts
      markdown.rs    Renders a parsed Markdown document
```

### Conventions

- **State** lives in the `AppState` entity, registered as a GPUI global so
  actions can reach it. Views observe it and re-render on `cx.notify()`.
- **Network calls** are blocking (`ureq`) and always run via
  `cx.background_executor().spawn(...)`. Results are applied back with
  `WeakEntity::update`. In-flight `Task`s are stored on the entity so they are
  cancelled when replaced or dropped.
- **GitHub data** comes from GraphQL. Add a query in `github/queries.rs`, map
  it to a type in `github/models.rs`, and expose a method on `GithubClient`.

### Design system

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
  navigation rows, `Badge` for status pills, `Callout` for errors and warnings,
  and `Spinner` for loading.

## Testing

```bash
cargo test
```

UI tests run Docket headlessly on GPUI's test platform (`#[gpui::test]`):
the window is never shown and input goes straight to it, so they don't
move the mouse, take focus, or need a display. `test_support::open_docket`
opens the workspace signed in to a `FakeGithub` seeded with the test's pull
requests and projects. Tests click elements by test selector
(`cx.click_on("Review requested")`), type keys (`cx.simulate_keystrokes("down
right")`), and check what's drawn (`cx.is_drawn(&pr.id)`) or the state
behind it (`state.read_with(cx, …)`). See `ui/workspace.rs` and
`ui/file_browser` for examples.

Components register their id as a test selector (`ListItem`, `Button`,
`IconButton`), as do PR rows (the PR's `id`). Give anything else a test
needs to find one with `.test_selector(|| …)`; it costs nothing outside
tests. GPUI's test platform measures text with placeholder metrics and
paints nothing, so tests check behavior and structure, not appearance.

Under `cfg(test)` nothing reaches your sign-in, projects, or GitHub: the
token store keeps nothing, `projects.json` isn't read or written, the
GitHub CLI and device flow refuse, and only a faked `GithubClient` answers.
Tests that need a checkout make one in a temp dir with
`test_support::git_checkout`. Avoid opening files in a language with a
server (Rust, TypeScript, Go, Python) in tests, since that starts the real
language server.

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

## Shortcuts

| Key      | Action                                           |
| -------- | ------------------------------------------------ |
| `cmd-o`  | Add project                                      |
| `cmd-}`  | Next project                                     |
| `cmd-{`  | Previous project                                 |
| `escape` | Dismiss a peek, hover, or selection, cancel setup or sign-in, or close a pull request or project's files |
| `up`/`down`, `left`/`right`, `enter` | Move through, collapse and expand the file tree |
| `cmd-c`  | Copy the selection in a diff or file             |
| `cmd-a`  | Select every line of a diff or file              |
| `cmd`-click, `f12` | Go to the definition of the symbol clicked, or at the selection |
| `ctrl--` | Go back to where the last jump to a definition came from |
| `cmd-r`  | Refresh                                          |
| `cmd-q`  | Quit                                             |

Sign Out is in the app menu.

## Not built yet

- PR detail tabs beyond the conversation and files (commits, checks)
- Diff follow-ups: split view, expanding context and revealing the enclosing
  function, word-level changes, keyboard navigation between files and hunks,
  syntax injections (code in Markdown, CSS in HTML), more LSP (references,
  choosing between several definitions), hover and definitions on removed
  lines, and highlighting code blocks in hovers
- File browser follow-ups: finding files by name, opening
  a file in the editor, and watching the checkout for changes instead of
  re-reading on refresh
- Review actions (approve, request changes, comment) via GraphQL mutations
- Pagination, background polling, and notifications
- Configurable/saved searches. Project scoping filters the first page of each
  inbox client-side, so it can miss PRs until pagination lands
- Renaming projects, and connecting a checkout without a GitHub remote
  (the text field now exists to build these on)
- Rate-limit handling and token-refresh/revocation UX
