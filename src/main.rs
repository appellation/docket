mod ai;
mod auth;
mod chrome;
mod config;
mod diff;
mod github;
mod http_client;
mod lsp;
mod markdown;
mod projects;
mod state;
mod syntax;
#[cfg(test)]
mod test_support;
mod ui;
mod updater;

use std::sync::Arc;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{Action, App, Application, KeyBinding, Menu, MenuItem, actions};

use crate::config::Config;
use crate::http_client::UreqHttpClient;
use crate::state::AppState;
use crate::ui::theme::{Theme, ThemeMode};
use crate::ui::{Assets, DiffView, FileBrowserView, text_input::TextInput, windows};

actions!(
    docket,
    [
        Quit,
        NewWindow,
        NewTab,
        CloseTab,
        Minimize,
        Zoom,
        Refresh,
        SignOut,
        DisconnectClaude,
        AddProject,
        NextProject,
        PreviousProject,
        Cancel,
        CheckForUpdates,
        RestartToUpdate,
        InstallChromeExtension,
        RestartLanguageServers,
        UseSystemAppearance,
        UseLightAppearance,
        UseDarkAppearance
    ]
);

/// Opens a link in the window it was clicked in: a pull request there,
/// anything else in the browser.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = docket, no_json)]
pub struct OpenLink {
    pub url: String,
}

fn main() {
    // Started by the browser as the Chrome extension's native messaging
    // host, which answers it over stdio rather than opening the app.
    if let Some(origin) = chrome::native_host::origin_arg() {
        std::process::exit(chrome::native_host::run(&origin));
    }
    let config = Config::load();

    let app = Application::new()
        .with_assets(Assets)
        .with_http_client(Arc::new(UreqHttpClient::new()));
    // Clicking the Dock icon with every window closed opens one. GPUI
    // calls this outside an update, so nothing would run something
    // deferred (`windows::new_window`) until something else updates the
    // app; with no window open, that may be never. Opening the window is
    // an update of its own.
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            windows::open(None, false, cx, |_, _| {});
        }
    });
    // Links from other apps (`docket://`, or github.com ones sent to
    // Docket). They can arrive before launching finishes, and the callback
    // has no `App`, so they wait on a channel.
    let (links_tx, mut links) = mpsc::unbounded();
    app.on_open_urls(move |urls| {
        links_tx.unbounded_send(urls).ok();
    });
    app.run(move |cx: &mut App| {
        init(cx);
        AppState::init(config, cx);
        windows::open(None, false, cx, |_, _| {}).expect("failed to open window");
        windows::set_launched(cx);
        cx.activate(true);
        cx.spawn(async move |cx| {
            while let Some(urls) = links.next().await {
                cx.update(|cx| windows::open_links(urls, cx)).ok();
            }
        })
        .detach();
        listen_to_extension(cx);
    });
}

/// Opens the pull requests the Chrome extension's host sends, answering
/// each once it's open or can't be. After the first window opens, so one
/// is there when the extension launched Docket.
fn listen_to_extension(cx: &mut App) {
    let Some(path) = chrome::socket::path() else {
        return;
    };
    let mut requests = match chrome::socket::listen(&path) {
        Ok(requests) => requests,
        Err(err) => {
            eprintln!("the Chrome extension can't reach Docket: {err:#}");
            return;
        }
    };
    cx.spawn(async move |cx| {
        while let Some(chrome::socket::OpenRequest { request, reply }) = requests.next().await {
            cx.update(|cx| {
                let opened = windows::open_from_extension(request, cx);
                cx.spawn(async move |_| reply.send(opened.await).ok())
                    .detach();
            })
            .ok();
        }
    })
    .detach();
}

/// The theme, actions, menus, and keybindings. Shared with the UI tests,
/// which set up their own `AppState`. Actions on what a window shows are
/// handled by its `Workspace`; these are the app's own.
fn init(cx: &mut App) {
    windows::init(cx);
    Theme::init(cx);
    TextInput::bind_keys(cx);
    DiffView::bind_keys(cx);
    FileBrowserView::bind_keys(cx);
    register_actions(cx);
}

fn register_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    // With no window open there's nothing to open from, so these are
    // the app's rather than a window's.
    cx.on_action(|_: &NewWindow, cx| windows::new_window(cx));
    cx.on_action(|_: &NewTab, cx| windows::new_tab(cx));
    cx.on_action(|_: &SignOut, cx| {
        AppState::global(cx).update(cx, |state, cx| state.sign_out(cx));
    });
    cx.on_action(|_: &DisconnectClaude, cx| {
        AppState::global(cx).update(cx, |state, cx| state.disconnect_claude(cx));
    });
    cx.on_action(|_: &CheckForUpdates, cx| {
        AppState::global(cx).update(cx, |state, cx| state.check_for_updates(true, cx));
    });
    cx.on_action(|_: &RestartToUpdate, cx| {
        AppState::global(cx).update(cx, |state, cx| state.restart_to_update(cx));
    });
    cx.on_action(|_: &UseSystemAppearance, cx| Theme::set_mode(ThemeMode::System, cx));
    cx.on_action(|_: &UseLightAppearance, cx| Theme::set_mode(ThemeMode::Light, cx));
    cx.on_action(|_: &UseDarkAppearance, cx| Theme::set_mode(ThemeMode::Dark, cx));

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-t", NewTab, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-o", AddProject, None),
        KeyBinding::new("cmd-}", NextProject, None),
        KeyBinding::new("cmd-{", PreviousProject, None),
        KeyBinding::new("escape", Cancel, None),
    ]);
    cx.set_menus(vec![
        Menu {
            name: "Docket".into(),
            items: vec![
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Add Project…", AddProject),
                MenuItem::action("Refresh", Refresh),
                MenuItem::action("Sign Out", SignOut),
                MenuItem::action("Disconnect Claude Code", DisconnectClaude),
                MenuItem::action("Install Chrome Extension…", InstallChromeExtension),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Window", NewWindow),
                MenuItem::action("New Tab", NewTab),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
            ],
        },
        // AppKit adds Show Tab Bar and Show All Tabs here.
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::submenu(Menu {
                    name: "Appearance".into(),
                    items: vec![
                        MenuItem::action("System", UseSystemAppearance),
                        MenuItem::action("Light", UseLightAppearance),
                        MenuItem::action("Dark", UseDarkAppearance),
                    ],
                }),
                MenuItem::separator(),
                MenuItem::action("Restart Language Servers", RestartLanguageServers),
            ],
        },
        // Named "Window", GPUI makes this the app's Windows menu: AppKit
        // adds the tab commands and lists the open windows.
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
        },
    ]);
}
