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

### App bundle (macOS)

`scripts/bundle.sh` builds `target/release/Docket.app`, with its icon and
the `docket://` URL scheme (arguments go to `cargo build`, e.g.
`--no-default-features`). macOS routes `docket://` links to it once it has
been opened, or moved to `/Applications`. `DOCKET_CHANNEL=main` builds one
that updates from `main` rather than stable releases (see Updates).

```bash
scripts/bundle.sh
```

### Releases

Releases are published on two channels, each with a disk image to install
from, `Docket-macos-<arch>.dmg` (`aarch64` for Apple silicon, `x86_64` for
Intel), and a zip that Docket updates from, `Docket-macos-<arch>.zip`:

- **Main**: the `main-latest` prerelease, at the tip of `main`. When `main`
  moves, the release and its tag are deleted and created again at the new
  commit.
- **Stable**: a release tagged `v<version>`, matching the version in
  `Cargo.toml`.

The release workflow (`.github/workflows/release.yml`) does both on
GitHub's macOS runners, building for Apple silicon and Intel:

- **Every push to `main`** publishes `main-latest`. A newer push cancels a
  build of an older one.
- **Publishing a release** (not a prerelease) builds that tag as stable
  and attaches both builds to it, usually about 15 minutes after publishing.
  So to cut one, bump the version in `Cargo.toml` on `main`, then create a
  release tagged `v<version>` on GitHub. The workflow fails if the tag
  doesn't match the version.

To install, open the disk image for your Mac from a release and drag
Docket onto Applications. Signed with a Developer ID, the image and the app
are notarized, so macOS opens them without a warning. The repository is
private, so download it signed in to GitHub, or with the GitHub CLI:

```bash
gh release download main-latest --repo appellation/docket --pattern 'Docket-macos-aarch64.dmg'
```

Builds precompile Metal shaders (`--no-default-features`), with Xcode 26.6
pinned on the macOS 26 runners. Its Metal compiler is a separate download,
so the workflow runs `xcodebuild -downloadComponent MetalToolchain` when
it's missing, and fails if there's still no compiler.

To publish this Mac's build by hand instead, `scripts/release.sh` builds
and uploads it (other arguments go to `scripts/bundle.sh`). It creates the
release if it isn't there, or adds this Mac's build to it.
`scripts/package.sh` and `scripts/publish.sh` are its two halves, which the
workflow runs on different machines.

```bash
scripts/release.sh --no-default-features
```

```bash
scripts/release.sh --channel main --no-default-features
```

## Updates

Docket updates itself from this repository's GitHub releases, on the
channel it was built for:

- **Stable** builds follow the latest release, and update when its version
  is higher than theirs.
- **Main** builds follow the `main-latest` prerelease, and update whenever
  it's at a commit other than the one they were built from.

While signed in, and running from `Docket.app`, Docket checks its channel's
release on launch and every hour after (a GraphQL query of about one point;
the repository may be private, so it uses your sign-in). An update with a
build for this Mac downloads in the background. Before it replaces
anything, Docket checks it against GitHub's size and SHA-256 for the
download, that it's `dev.docket` on the same channel, at the release's
version (stable) or commit (main), and that its code signature is intact.
If the running Docket is signed with a certificate, the update must be
signed with the same one (see Changing certificates). An update signed with a Developer ID must also
pass Gatekeeper: notarized, with neither the certificate nor the
notarization revoked. Then it moves the new bundle into place of the one
running, and the sidebar offers **Restart to Update**. Quitting and
opening Docket again does the same.

**Check for Updates…** in the app menu checks now, and shows what it
found, including that you're up to date or why it couldn't check. Checks
on their own stay quiet unless there's an update. Where Docket can't
install one itself (from `cargo run`, with no build for this Mac, or in a
folder you can't write to, like a translocated app run from Downloads),
the sidebar links to the release instead.

The channel is fixed when Docket is built. To switch, install a build from
the other channel's release; both are `dev.docket`, so they share your
sign-in and projects, and only one is installed at a time.

### Signing

Without a certificate, builds are signed ad hoc. An ad hoc signature
changes with every build, so macOS asks for keychain access to your sign-in
again after each update, and updates can only check that they're intact,
not who signed them. Signing with a certificate fixes both: macOS trusts
the app by its certificate, and Docket then only updates to builds signed
with the same one.

- **Developer ID** (Apple Developer Program, $99 a year): create a
  **Developer ID Application** certificate in Xcode → Settings → Accounts →
  Manage Certificates. Builds signed with it must be notarized, which also
  lets macOS open them without a warning when they're downloaded in a
  browser. Renewing it keeps the same Team ID, which is what updates check.
- **Self-signed** (free): Keychain Access → Certificate Assistant → Create a
  Certificate…, with Identity Type **Self Signed Root** and Certificate Type
  **Code Signing**. Updates and keychain access work the same, but macOS
  blocks the first install from a browser download until it's allowed in
  System Settings → Privacy & Security. Replacing the certificate, or
  moving to a Developer ID, means installing Docket again by hand (see
  Changing certificates).

Locally, `DOCKET_SIGN_IDENTITY` (a name or SHA-1 from `security
find-identity -p codesigning`) signs with the hardened runtime and a secure
timestamp. With a Developer ID, `DOCKET_NOTARY_KEY` (an App Store Connect
API key's `.p8`), `DOCKET_NOTARY_KEY_ID`, and `DOCKET_NOTARY_ISSUER`
notarize and staple it too:

```bash
DOCKET_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" scripts/release.sh
```

The release workflow does the same from secrets, listed at the top of
`.github/workflows/release.yml`: the certificate as a base64 `.p12` with
its password, and the notary key. Without them it signs ad hoc. A build
signed with a Developer ID but not notarized fails, since Docket wouldn't
update to it.

### Renewing the certificate

A Developer ID certificate lasts five years. Every Monday,
`.github/workflows/certificate-expiry.yml` reads the one in the secrets,
and within 60 days of its expiry opens an issue, labeled
`certificate-renewal`, with the steps to renew it; once it has expired, the
run fails too. Renewing is by hand, since only the Apple Developer
account's Account Holder can create a Developer ID certificate: create a
new one from a certificate signing request, export it as a `.p12`, and
replace `DOCKET_SIGNING_CERTIFICATE` and
`DOCKET_SIGNING_CERTIFICATE_PASSWORD`. It has the same Team ID, so
installed copies keep updating. Don't revoke the old one, or builds signed
with it stop installing. Running the workflow again (`gh workflow run
certificate-expiry.yml`) closes the issue. To check a `.p12` yourself:

```bash
CERTIFICATE_PASSWORD=… scripts/certificate-expiry.sh Certificates.p12
```

### Changing certificates

Builds signed ad hoc have no signer to keep, so they update to any validly
signed build, and the first certificate needs nothing. After that, an
installed Docket only updates to builds whose signer meets its own: the
same Team ID for a Developer ID (so renewing is fine), the same certificate
for a self-signed one. Nothing moves installs to another signer, so
changing it (to another self-signed certificate, from self-signed to a
Developer ID, or to another team) means installing a build signed with the
new one by hand. Pushing to `main` or publishing a release is never enough
to change who an installed Docket trusts. After installing it, macOS asks
once for keychain access to the sign-in.

### Protecting the signing key

Faking an update takes both the signing key and a way to publish to this
repository's releases. With a Developer ID, it also takes the notary key,
and Apple can revoke either one.

The secrets are read from the repository unless the repository variable
`DOCKET_RELEASE_ENVIRONMENT` names an environment. Then they're read from
that environment's secrets, which can be limited to runs from `main` and
`v*` tags, so a workflow edited on another branch can't read them. A private
repository needs GitHub Pro for this, and for rulesets that keep `v*` tags
to maintainers. To set it up, with the secrets moved into the environment:

```bash
gh api -X PUT repos/appellation/docket/environments/release --input - <<'JSON'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
JSON
gh api repos/appellation/docket/environments/release/deployment-branch-policies -f name=main -f type=branch
gh api repos/appellation/docket/environments/release/deployment-branch-policies -f name='v*' -f type=tag
gh variable set DOCKET_RELEASE_ENVIRONMENT --body release
```

If a key leaks:

- **Developer ID:** ask Apple Developer Support to revoke the certificate,
  dated to when it leaked so earlier builds stay valid. Docket checks
  Developer ID updates with Gatekeeper, which asks Apple about revocations,
  so builds signed with it stop installing. Create a new Developer ID
  certificate, replace the secrets, and publish a build. It has the same
  Team ID, so installed copies update to it on their own. Rotate the App
  Store Connect key too, and review who can publish releases.
- **Self-signed:** there's no way to revoke it. Installed copies keep
  accepting builds signed with it, so everyone has to install a build
  signed with a new one by hand. Use a
  Developer ID for anything others install.

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
  repositories, and grant read and write **Pull requests** (to mark files as
  viewed), read-only **Contents**, **Commit statuses**, and **Issues**
  (repository), and read-only **Members** (organization). With read-only
  **Pull requests**, files marked as viewed stay in Docket rather than
  reaching GitHub.
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
or in all of them. Each project's default branch is pinned to the top;
below it, the rest is grouped by what it's waiting on, each group most
recently updated first. The headings show only when there's more than one
group:

- **The default branch**, one per project. Where it's checked out, that's
  what opens; otherwise the remote's (as of your last fetch), or the local
  one if the remote has none.
- **Review requested.** Open PRs where your review is requested directly.
- **Ready to merge.** Your open PRs that are approved.
- **Waiting on review.** Your other open PRs that aren't drafts, including
  those with changes requested.
- **Drafts.** Your draft PRs.
- **Involved.** Other people's open PRs you're involved in (commented on,
  mentioned, assigned), where your review isn't requested.
- **Branches.** Your branches that don't have a PR yet. PRs show however
  long they've been quiet, but only branches committed to in the last week
  do; **Show more** at the bottom goes back another week of branches each
  time, while there are older ones. Choosing a project starts from the last
  week again.

A branch is a local branch in the project's checkout with a commit on it,
authored or committed by your `user.email`, that isn't on the default
branch (the remote's `HEAD` for the project's repository, else `main` or
`master`). Branches already merged into the default branch are left out,
and so are those whose upstream is gone, as a squash-merged branch's is
once GitHub deletes it. A branch drops out once a PR in your docket is from
it, matched by name (locally or on its remote) or by its tip. Its date is
when its tip was committed. Branches are re-read from the checkout on every
refresh.

Clicking a PR opens it, and clicking a branch opens its changes (see
Branches below); the default branch opens its files instead (see Browsing
files). Either way the docket moves into a sidebar beside it.

### Merging and closing

An open PR's row, in the docket or its sidebar, has merge and close buttons
that show while the mouse is over it (and always on the open PR's row).
Each asks first. Merging uses your default merge method for the
repository, as GitHub's merge button does, and only if the PR's head is
still the commit the docket shows; drafts can't be merged. A merged or
closed PR leaves the docket at once, though GitHub's search takes a few
seconds to catch up. If GitHub refuses (branch protection, conflicts, no
permission), the docket says why.

Where a bot merges PRs, a project can merge by comment instead (see
Projects): merging leaves one of the project's comments, chosen at random,
and the PR stays in the docket, marked as commented on, until the bot
merges it.

### Staying up to date

The docket and an open pull request follow GitHub on their own. GitHub
can't push changes to an app without a server, so Docket polls, quietly
(what's shown stays until what replaces it arrives) and cheaply:

- The docket's searches run again every minute, and the projects' branches
  are re-read with them.
- An open pull request is checked every 15 seconds while its window is
  active, with a query of about one point of GitHub's rate limit that
  changes whenever anything the detail shows does (comments, reviews,
  pushes, resolved threads, merge state, checks). Only then is the detail
  fetched again, and its diff reloads if its head or base moved. The same
  query brings which files you've marked as viewed, which don't count as
  changes (with more than 100 files, they're fetched separately).

Switching back to a window catches up at once on whatever is due. That's
about 420 of GitHub's 5,000 points an hour with a pull request open. The
refresh button on the docket and on a pull request (`cmd-r`) loads
everything again now.

### Branches

A branch opens like a pull request, with the same tabs but for the
conversation: Files, what it changes since it left the default branch,
beside its changed files, and Commits, the commits on it since then, as a
PR from it would show them. Its title is its latest commit's subject. The diff is read from
Git at the branch's tip and compared to the default branch as the remote has
it (as of your last fetch), from the commit where they last met, so what's
landed on the default branch since doesn't show. Uncommitted changes don't
show either, even where the branch is checked out. Each refresh re-reads the
branches, and the diff and commits reload if the branch or the default
branch moved. Up to 250 of its latest commits are listed.

Hover and go to definition work as in a PR (see Files, Hover, and Go to
definition below): servers run in Docket's worktree at the branch's tip, and
start without asking, since the branch is yours.

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

The settings also choose how the project's PRs merge from the docket: on
GitHub, or by commenting, for repositories where a bot merges (a merge
queue that takes commands, say). Commenting needs at least one comment,
separated by commas (`/merge, ship it`); each merge leaves one of them at
random. Switching back to merging on GitHub keeps the comments for next
time.

Projects are stored locally in `projects.json` under the platform config dir
(`~/Library/Application Support/dev.docket/` on macOS), independent of the
signed-in account.

### Browsing files

Clicking the default branch in the docket shows its files: a directory tree
beside the selected file, highlighted as in the Files tab, with the docket
collapsed to a rail beside them. Where it's checked out, in the project's
checkout or one of its `git worktree`s, it shows as it is on disk there,
uncommitted changes and untracked files included. Otherwise Docket checks
out its tip in its own worktree for the project (in its cache, without a
branch, the same one pull requests' language servers use), leaving your
checkout as it is, and shows that. Either way the files are on disk, so
hover, definitions, and symbols work everywhere. The tree's header names
the branch.

Until a file is open, the main panel lists a directory, starting at the
root. Clicking a directory there (or in the tree) lists it instead, and the
path above links to each directory it's in, the open file's too, so you can
walk the checkout without the tree, which expands to follow.

A pull request's and a branch's Browse tab shows all their files the same
way, beside the pull request or branch: a pull request's as of its head
(fetched with its diff), and a branch's where it's checked out, or else as
of its tip, both checked out in Docket's worktree. That's where their
diff's language servers run, so the Browse tab shares them, and for a pull
request from a fork, servers that can run its code wait for you to allow
them, as in the diff. They follow the head when it moves. In the Files
tab, clicking a file's path opens it in the Browse tab. Docket keeps one
worktree per project, so two tabs on different branches of a project that
neither is checked out on share it, and the one opened last wins.

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

### Searching files

A field atop the tree searches the files three ways, and while it holds a
query the results show in the tree's place. The buttons under it switch
between them, keeping the query:

- **Files** (`cmd-p`): paths, matched fuzzily as fzf matches them: each
  word of the query a run of characters in order, ignoring case unless it
  has a capital, best match first. Opening one closes the search.
- **Text** (`cmd-shift-f`): lines of the files, grouped by file, as
  ripgrep finds them: literally, ignoring case unless the query has a
  capital, skipping binary files, symlinks, and files over 5 MB. Buttons
  beside the modes match case and take the query as a regular expression
  (Rust's syntax, as ripgrep's). It shows the first 2,000 lines.
- **Symbols** (`cmd-shift-t`): functions, types, and the like, from the
  language servers for the project's languages, which this starts. As each
  finishes loading, the search runs again, since it knows more.

`up`/`down` move through the results and `enter` opens one. A line or
symbol opens selected, and the results stay, to open the next; `ctrl--`
goes back. `escape` clears the search.

The files searched are those the tree lists. On disk, Docket finds them as
fd does, walking the checkout in parallel and skipping what Git ignores,
when a search first needs them, and again after a refresh. Text is searched
with ripgrep's own libraries on a thread per core. Tracked files you
haven't changed are read from Git's object store rather than opened one by
one, which on macOS is several times faster: in a checkout of 216,000 files,
finding a file by name takes about 2 ms, and searching all of their text
about 0.6 s (ripgrep itself takes 3.7 s there). Changed and untracked files
are read from disk, so edits you haven't committed show.

`cmd-f` finds in the open file, with a bar above its code: matches are
marked in the lines, the current one brighter, and the count beside the
field says which it's on. A selection within a line starts as the query,
which matches as text search does, with the same match-case and regex
buttons. `enter` (or `cmd-g`, `down`) moves to the next match and
`shift-enter` (or `cmd-shift-g`, `up`) to the previous, wrapping at the
ends; the first is the first from where you are in the file. The bar stays
open as you open other files, and `escape` closes it.

`cmd-shift-o` goes to a symbol in the open file: the same bar, in its
other mode, lists the file's outline from its language server, nested,
from the symbol you're in. Typing ranks the names fuzzily, as above. `up`
and `down` move through the list, scrolling the file to each symbol with
its name marked; `enter` or a click goes to one, selecting its name.
`escape` closes the list and puts the file back where it was. A file with
no language server says so.

## Pull request detail

Clicking a PR opens its conversation, loosely after GitHub's: the
description, the timeline, and (while open) where it stands on reviews and
merging, with reviewers, checks, and assignees alongside. Checks are grouped
by status (failing, in progress, successful, then skipped); passing and
skipped groups start collapsed, and a group of more than five scrolls.

The timeline is trimmed toward signal:

- Runs of bookkeeping events (review requests, deployments, labels,
  assignments, force-pushes, …) collapse into one expandable row. Merges,
  closes, reopens, and draft changes stay visible.
- Consecutive commits collapse into one row.
- Bot comments and hidden comments collapse to a one-line preview. Bot
  *reviews* stay expanded: they're feedback on the code.
- Comment reviews whose threads are all resolved or outdated collapse to a
  one-line preview, and join the runs above. Approvals and requests for
  changes stay.
- Resolved and outdated review threads start collapsed.

Up to 1,000 timeline items load. Bodies render as Markdown; HTML in them is
reduced to its text, and HTML comments are dropped.

A link in them to a pull request (`github.com/{owner}/{repo}/pull/{n}`)
opens it in the same tab, whether or not it's in the docket; one to its
commits or files (`…/commits`, `…/files`) opens that tab. Other links open
in the browser.

### Links from other apps

Docket opens pull request links from other apps in its front window (or a
new one), each after the first in a new tab. They arrive either as
`docket://` links (the github.com URL with `docket` for its scheme, e.g.
`docket://github.com/acme/widgets/pull/7`, which needs the app bundle) or
as github.com links sent to Docket, e.g. by
[Finicky](https://github.com/johnste/finicky) or `open -a Docket <url>`. A
link that arrives before you've signed in opens once you have.

Only pull requests open. Other links are dropped rather than sent back to
the browser, so route only `github.com/*/*/pull/*` to Docket: a router
sending it more than that could bounce them back and forth otherwise.

### Commits

The Commits tab lists the PR's commits, oldest first and grouped by day,
each opening on GitHub. They come from the timeline, so a PR whose timeline
is too long to load in full says how many are missing, with a link to the
rest on GitHub.

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

Beside the diff, the changed files are in a tree of their directories, as
in the file browser: each directory's subdirectories, then its files, with
top-level files last. A chain of directories holding nothing but one
another is one row, so `3rdparty/python` heads `py_universes.bzl` and,
nested under it, `api` with `uv.lock`. Clicking a directory collapses or
expands everything under it. The diff lists files in the same order.

Clicking a file's header collapses or expands it. Checking Viewed in its
header marks it as viewed: it collapses, gets a check in the tree, and
counts toward the tree's "2/5 viewed". It stays collapsed, in every window
and after a relaunch, until it changes. Then it expands again, marked
"Changed since viewed" until you check it again.

A pull request's viewed files are GitHub's: checking one in Docket checks
it on github.com, and one checked or unchecked there follows in Docket
when the pull request is next checked for changes (every 15 seconds, or on
switching back to its window). GitHub decides when a file has changed since
it was viewed. If GitHub refuses a mark (a fine-grained token without write
access to pull requests, say), Docket keeps it, shows why, and offers to
try again.

A branch without a pull request has viewed files of its own, which change
when what the diff shows of them does: a new commit that touches a file's
changes, not one that only moves lines elsewhere in it. These, and the last
of GitHub's states Docket saw, are kept on disk under
`~/Library/Application Support/dev.docket/viewed.json`, and forgotten
after 180 days untouched.

### Changes grouped by Claude

Connect Claude Code and a diff (a PR's or a branch's) shows grouped by change
rather than file by file. Claude Haiku 5.5, run in Claude Code, sorts the
diff's hunks into the logical changes they make, across files (a function and
its callers, a type and the code that adopts it), in the order a reviewer
should read them. In the diff, each group is headed by its title, a short summary of what changes,
and how it affects the rest of the code, then its hunks file by file. The
changed-files tree lists the groups' titles, each with a tree of its files
under it; a file with hunks in several groups is under each. Clicking a
group scrolls the diff to it, its chevron collapses its files, and clicking
a file scrolls to its hunks in that group. The toolbar above the diff
switches between By Change and By File.

Claude Code runs in a copy of the repository at the diff's head (a worktree
in Docket's cache, so your checkout is left alone), and reads the code
around the changes as it needs to (the callers of a changed function, the
definition of a type, the tests), so groups and their impact reflect more
than the diff shows.

To connect, install [Claude Code](https://claude.com/claude-code), sign in
with `claude auth login` in a terminal, then choose Group Changes with
Claude… above any diff and Use Claude Code. Docket runs your `claude` with
its sign-in, and stores no credential of its own. Docket → Disconnect Claude
Code turns it off.

Each run is read-only: Claude Code gets only its Read, Grep, and Glob tools,
confined to the worktree, and ignores settings files (so a PR can't bring
hooks or permission rules of its own), MCP servers, and slash commands. Runs
aren't saved to Claude Code's session history. A trusted author's pull
request may also have Claude read the repository's history, with `git log`,
`git show`, `git blame`, and `git diff`, to see why the code it changes was
written. Trusted means the author is a member of the organization that owns
the repository (or owns it), or is you; your local branches are trusted
too. Any other command is still denied. There's no Docket server:
the diff goes from Claude Code to Anthropic as it would for any of your own
sessions. A diff is analyzed once, as it loads, and the analysis is kept (in
memory, and on disk under `~/Library/Caches/dev.docket/analyses`) until the
diff changes: a new head or base, or a new commit on a branch. Hunks over 400
lines are cut short in what's sent, and diffs of more than about 600 KB
aren't analyzed.

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
| `escape` | Dismiss a peek, hover, or selection, close find or clear a file search, cancel setup or sign-in, or close a pull request, branch, or files |
| `up`/`down`, `left`/`right`, `enter` | Move through, collapse and expand the file tree |
| `cmd-p`, `cmd-shift-f`, `cmd-shift-t` | Search the browsed files by name, text, or symbol |
| `cmd-f`, `cmd-g`, `cmd-shift-g` | Find in the open file; next and previous match |
| `cmd-shift-o` | Go to a symbol in the open file |
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

Check for Updates… and Sign Out are in the app menu.

## Not built yet

- PR detail tabs beyond the conversation, commits, and files (checks)
- Diff follow-ups: split view, expanding context and revealing the enclosing
  function, word-level changes, keyboard navigation between files and hunks,
  syntax injections (code in Markdown, CSS in HTML), more LSP (references,
  choosing between several definitions), hover and definitions on removed
  lines, and highlighting code blocks in hovers
- Analysis follow-ups: letting Claude search the checkout for a change's
  callers (impact is judged from the diff alone today), and the PR's title
  and description as context
- File browser follow-ups: opening a file in the editor, watching the
  checkout for changes instead of re-reading on refresh, and a worktree per
  tab rather than per project
- Review actions (approve, request changes, comment) via GraphQL mutations
- Pagination, and notifications
- Configurable/saved searches. Project scoping filters the first page of each
  search client-side, so it can miss PRs until pagination lands
- Branch follow-ups: branches pushed to GitHub that aren't in the local
  checkout, uncommitted changes in a checked-out branch's diff, browsing a
  branch other than the default's files, and opening a pull request from one
- Renaming projects, and connecting a checkout without a GitHub remote
- Link follow-ups: going back to the pull request a link was followed
  from, and scrolling to a linked comment, commit, or file
- Tab follow-ups: opening a pull request in a new tab, and restoring tabs
  on relaunch
- Rate-limit handling and token-refresh/revocation UX
