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
on launch. Requested scopes: `repo`, `read:org`.

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
an org whose SSO the token isn't authorized for, so the docket can look empty
when it isn't. On each refresh Docket checks every project's repository and,
where GitHub hides it, flags the project and explains why above the PR list,
with a link to fix it. Orgs with no project in Docket aren't checked.

## The docket

The list beside the sidebar is everything pending in the selected project,
or in all of them. The headings show only when there are both review
requests and other items:

- **Review requested.** Open PRs where your review is requested directly,
  most recently updated first.
- **The default branch**, pinned below them, one per project. Where it's
  checked out, that's what opens; otherwise the remote's (as of your last
  fetch), or the local one if the remote has none.
- **Recently updated.** Below that, the other open PRs you wrote or are
  involved in, together with your branches that don't have a PR yet, most
  recently updated first.

A branch is a local branch in the project's checkout with a commit on it,
authored or committed by your `user.email`, that isn't on the default
branch (the remote's `HEAD` for the project's repository, else `main` or
`master`). Branches already merged into the default branch are left out,
and so are those whose upstream is gone, as a squash-merged branch's is
once GitHub deletes it. A branch drops out once a PR in your docket is from
it, matched by name (locally or on its remote) or by its tip. Its date is
when its tip was committed. Branches are re-read from the checkout on every
refresh.

Clicking a PR opens it; clicking a branch opens its files (see Browsing
files). Either way the docket moves into a sidebar beside it.

## Projects

A project is a local Git checkout connected to the GitHub repository its PRs
live in. Selecting one in the sidebar scopes the docket to that repository;
the checkout is where diffs are read from and language servers find the
project's toolchain.

Adding a project (`cmd-o`) asks for a folder, resolves it to the checkout
root, reads its GitHub remotes, and checks each on GitHub. It pre-selects the
most likely target: an `upstream` remote or a fork's parent over `origin`.

The selected project's header names its repository and folder; clicking the
folder shows it in Finder. Its settings button reopens the connection,
where you can choose another folder or repository, or remove the project
(your checkout isn't touched). If the folder has moved, the settings say so
and offer both.

Projects are stored locally in `projects.json` under the platform config dir
(`~/Library/Application Support/dev.docket/` on macOS), independent of the
signed-in account.

### Browsing files

Clicking a branch in the docket shows its files: a directory tree beside the
selected file, highlighted as in the Files tab, with the docket collapsed
to a rail beside them. A branch that's checked out, in the project's
checkout or one of its `git worktree`s, shows as it is on disk there,
uncommitted changes and untracked files included. Any other branch shows as
it is at its tip, read from Git without checking it out, and has no
language servers. The tree's header names the branch.

On disk, a directory shows what `git ls-files --cached --others --exclude-standard`
would: tracked entries still on disk, plus untracked ones Git doesn't ignore.
So `target/` and `node_modules/` stay out, but a tracked file in an ignored
directory doesn't, and an untracked folder holding only ignored files (a
cache that ignores itself) doesn't show either. Submodules and nested
repositories show as one entry. Ignore rules come from the `.gitignore`s
from the repository's root down, `.git/info/exclude`, and the global excludes
file named in `~/.gitconfig` (or Git's default). Nothing else, like hidden
files or `.ignore`, is skipped. Large checkouts open quickly: each directory
is listed the first time it's expanded.

The tree is keyboard-driven: `up`/`down` move through it and show each file
they reach, `right` expands a directory (or moves into an expanded one),
`left` collapses it (or moves to the one it's in), and `enter` toggles one.
Files have the same limits as diffs: over 5 MB or binary shows a notice, and
over 1 MB or 20,000 lines shows plain text. `cmd-r` re-reads the index, the
expanded directories, and the open file.

The open file's code selects, copies, hovers, and jumps to definitions just
as in the diff (see Files, Hover, and Go to definition below). Its language
servers run in the checkout (or worktree) itself, since that's the version
being read, rather than in Docket's worktree. Each starts the first time a file in its
language opens, and the status of each shows below the tree. A checkout is
your own working copy, as your editor would see it, so every server starts
without asking, including those that run project code. They stop when the
browser closes. rust-analyzer builds in your checkout's `target/`, so it can
wait on a cargo lock while your editor or a build holds one. When a refresh
finds a file changed, its server gets the new text.

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

Up to 1,000 timeline items load. Bodies render as Markdown; HTML in them is
reduced to its text, and HTML comments are dropped.

### Files

The Files tab shows the PR's diff, loaded the first time the tab opens so
the conversation never waits on a fetch. Diffs come from the project's local
checkout, not GitHub's patch API, so Docket has both sides of every file in
full: syntax highlighting sees real context, and language servers see real
files. A PR in a repository with no project offers to add one.

If the PR's head or base commit isn't in your checkout, Docket fetches it
from the remote that points at the project's repository (or its GitHub URL)
into `refs/docket/pull/<n>/head` and `refs/docket/pull/<n>/base`. That
namespace leaves your branches and `FETCH_HEAD` alone. A fetch that needs
credentials fails rather than prompting. The diff runs from the merge base
to the head, as on GitHub, with renames detected, and shows changes with
three lines of context. Files over 5 MB and binary files show a notice
instead.

Lines render plain until highlighting finishes. Languages: Rust,
TypeScript/TSX, JavaScript/JSX, Python, Go, JSON, TOML, YAML, shell, CSS,
HTML, and Markdown.

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
   doesn't install servers.
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
file rather than a regex: `impl AppState › load_diff`. Only scopes that
contain the excerpt's first change, and whose names are above the excerpt,
are shown. A scope whose start the excerpt already shows isn't repeated.
Each name hovers like code, showing the server's hover for its declaration.

Outlines come from the same servers as hover, once each is running, for
files on the new side whose excerpts start partway in. Names appear a few
seconds after the diff. Deleted files, and anonymous scopes some servers
leave out of outlines (TypeScript's `export default { … }`), get none.

### Go to definition

Command-clicking code asks its language server where the symbol is defined;
so does `F12`, for the word at the start of the selection. Holding command
over code a running server can answer for underlines it as a link. Wherever
hover works, this does too, including the scope names in hunk headers.

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

Command-option-click (or `option-F12`) opens a definition in the project's
checkout in a new tab instead, browsing the project's files there with the
definition selected, and leaves the view it came from as it was. From the
diff, that's your checkout's version of the file, which may not be at the
PR's head. Definitions outside the checkout peek as usual.

## Windows and tabs

Each window is independent: its own project, and open pull request or
branch, with its own language servers. On macOS windows are native tabs.
`cmd-t` (or the tab bar's "+") opens a tab beside the current one,
starting on the same project; `cmd-n` opens a window, which joins as a tab if System Settings →
Desktop & Dock → "Prefer tabs when opening documents" says to. The Window
menu has macOS's own tab commands (Show All Tabs, Merge All Windows, Move
Tab to New Window, and `ctrl-tab` to switch), and each tab is titled with
what it shows. The sign-in, pull requests, projects, and branches are shared, so a refresh
or a new project in one shows in all of them.

## Shortcuts

| Key      | Action                                           |
| -------- | ------------------------------------------------ |
| `cmd-o`  | Add project                                      |
| `cmd-}`  | Next project                                     |
| `cmd-{`  | Previous project                                 |
| `escape` | Dismiss a peek, hover, or selection, cancel setup or sign-in, or close a pull request or branch's files |
| `up`/`down`, `left`/`right`, `enter` | Move through, collapse and expand the file tree |
| `cmd-c`  | Copy the selection in a diff or file             |
| `cmd-a`  | Select every line of a diff or file              |
| `cmd`-click, `f12` | Go to the definition of the symbol clicked, or at the selection |
| `cmd-option`-click, `option-f12` | Open the definition in a new tab |
| `ctrl--` | Go back to where the last jump to a definition came from |
| `cmd-r`  | Refresh                                          |
| `cmd-t`  | New tab                                          |
| `cmd-n`  | New window                                       |
| `cmd-w`  | Close the tab                                    |
| `cmd-m`  | Minimize                                         |
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
  a file in the editor, watching the checkout for changes instead of
  re-reading on refresh, and language servers for branches that aren't
  checked out
- Review actions (approve, request changes, comment) via GraphQL mutations
- Pagination, background polling, and notifications
- Configurable/saved searches. Project scoping filters the first page of each
  search client-side, so it can miss PRs until pagination lands
- Branches pushed to GitHub that aren't in the local checkout
- Renaming projects, and connecting a checkout without a GitHub remote
- Tab follow-ups: opening a pull request in a new tab, and restoring tabs
  on relaunch
- Rate-limit handling and token-refresh/revocation UX
