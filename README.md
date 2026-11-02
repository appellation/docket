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

## Layout

```
src/
  main.rs            App bootstrap, window, actions, menus, keybindings
  config.rs          OAuth client ID, scopes, API URLs
  state.rs           AppState entity: auth status, PR sections, async tasks
  auth/
    device_flow.rs   Device-code request and token polling
    token_store.rs   Keychain persistence
  github/
    client.rs        Blocking GraphQL client (run on the background executor)
    queries.rs       GraphQL documents and wire types
    models.rs        Domain types used by the UI
  ui/
    workspace.rs     Root view; switches between sign-in and dashboard
    sign_in.rs       Device-flow screen
    sidebar.rs       User + inbox sections
    pr_list.rs       PRs for the selected section
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

| Key     | Action  |
| ------- | ------- |
| `cmd-r` | Refresh |
| `cmd-q` | Quit    |

Sign Out is in the app menu.

## Not built yet

- PR detail view (diff, files, timeline, checks)
- Review actions (approve, request changes, comment) via GraphQL mutations
- Pagination, background polling, and notifications
- Configurable/saved searches and repo filters
- Rate-limit handling and token-refresh/revocation UX
