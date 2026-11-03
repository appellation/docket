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
repository, which scope the inboxes), and a sidebar of PR inboxes (review
requested, authored, involved) with a PR list, a PR detail view showing the
conversation and diff, and a browser for a project's local files. The "Not
built yet" section of `README.md` tracks the near-term gaps; the goals above
are the longer arc.

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

## Conventions

- **State** lives in the `AppState` entity, registered as a GPUI global so
  actions can reach it. Views observe it and re-render on `cx.notify()`.
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
  navigation rows, `Badge` for status pills, `Callout` for errors and warnings,
  `Spinner` for loading, and `TextInput` (`ui/text_input.rs`) for single-line
  fields.

## Testing

`cargo test` runs everything. Check UI changes with a UI test rather than by
launching the app.

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

Under `cfg(test)` nothing reaches the user's sign-in, projects, or GitHub:
the token store keeps nothing, `projects.json` isn't read or written, the
GitHub CLI and device flow refuse, and only a faked `GithubClient` answers.
Tests that need a checkout make one in a temp dir with
`test_support::git_checkout`. Avoid opening files in a language with a
server (Rust, TypeScript, Go, Python) in tests, since that starts the real
language server.

## Implementation notes

### Pull request detail

The detail is one GraphQL query plus up to nine more pages of timeline
(1,000 items). Bodies are parsed as Markdown on the background executor;
HTML is reduced to its text and HTML comments are dropped.

### Diffs

Loading a diff (`diff/git.rs`):

1. If the PR's head or base commit isn't local, fetch it into
   `refs/docket/pull/<n>/head` and `refs/docket/pull/<n>/base`. Git runs with
   `GIT_TERMINAL_PROMPT=0`, so it fails instead of waiting on a credential
   prompt.
2. Diff the merge base against the head, detecting renames.
3. Read both sides of each file with `git cat-file --batch`.

`diff/` aligns every line of the old and new file once; excerpts (changes
with three lines of context) choose what's visible. Having both sides in
full is what expanding context, editor integration, and LSP positions build
on.

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

Files load and highlight on the background executor with the same limits as
diffs. The open file's code view is `ui/code`, shared with the diff.

### Language servers

Requests used: `textDocument/hover` for hover, `textDocument/documentSymbol`
for hunk-header scopes, and `textDocument/definition` for go to definition.
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
