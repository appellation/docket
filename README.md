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
src/
  main.rs            App bootstrap, window, actions, menus, keybindings
  config.rs          OAuth client ID, scopes, API URLs
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
    components.rs    Shared button, path formatting
    theme.rs         Color palette
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
