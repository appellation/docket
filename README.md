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
- **Personal access token.** A classic token needs the `repo` and `read:org`
  scopes; a fine-grained token needs the org as resource owner. Either may
  need SSO authorization, or org approval for fine-grained tokens, depending
  on the org's policies. Stored in the keychain once GitHub accepts it.

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
  state/
    mod.rs           AppState entity: auth status, PR sections, async tasks
    projects.rs      Project selection and the add/edit setup flow
  auth/
    device_flow.rs   Device-code request and token polling
    github_cli.rs    Token from the GitHub CLI's login
    token_store.rs   Keychain (or, in debug builds, file) persistence of the sign-in
  github/
    client.rs        Blocking GraphQL client (run on the background executor)
    queries.rs       GraphQL documents and wire types
    models.rs        Domain types used by the UI
  projects/
    git.rs           Reads a checkout's root and GitHub remotes
    store.rs         projects.json persistence
  ui/
    workspace.rs     Root view; switches between sign-in and dashboard
    sign_in.rs       Sign-in screen (device flow, GitHub CLI, token)
    text_input.rs    Single-line text field (adapted from GPUI's example)
    sidebar.rs       User, projects, and inbox sections
    pr_list.rs       Project header + PRs for the selected section
    project_setup.rs Connect a folder to its GitHub repository
    theme.rs         Semantic color tokens, light and dark palettes
    components/
      icon.rs        Lucide icons, asset source, spinner
      button.rs      Button, IconButton, tooltip
      list.rs        ListItem, badges, section headings, callouts
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
  `merged`) each with a `*_subtle` fill. It follows the system appearance;
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

| Key      | Action                   |
| -------- | ------------------------ |
| `cmd-o`  | Add project              |
| `cmd-}`  | Next project             |
| `cmd-{`  | Previous project         |
| `escape` | Cancel setup or sign-in  |
| `cmd-r`  | Refresh                  |
| `cmd-q`  | Quit                     |

Sign Out is in the app menu.

## Not built yet

- PR detail view (diff, files, timeline, checks)
- Review actions (approve, request changes, comment) via GraphQL mutations
- Pagination, background polling, and notifications
- Configurable/saved searches. Project scoping filters the first page of each
  inbox client-side, so it can miss PRs until pagination lands
- Renaming projects, and connecting a checkout without a GitHub remote
  (the text field now exists to build these on)
- Rate-limit handling and token-refresh/revocation UX
