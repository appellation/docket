mod auth;
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

use std::sync::Arc;

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
        AddProject,
        NextProject,
        PreviousProject,
        Cancel,
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
    let config = Config::load();

    let app = Application::new()
        .with_assets(Assets)
        .with_http_client(Arc::new(UreqHttpClient::new()));
    // Clicking the Dock icon with every window closed opens one.
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            windows::new_window(cx);
        }
    });
    app.run(move |cx: &mut App| {
        init(cx);
        AppState::init(config, cx);
        windows::open(None, false, cx, |_, _| {}).expect("failed to open window");
        cx.activate(true);
    });
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
                MenuItem::action("Add Project…", AddProject),
                MenuItem::action("Refresh", Refresh),
                MenuItem::action("Sign Out", SignOut),
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
            items: vec![MenuItem::submenu(Menu {
                name: "Appearance".into(),
                items: vec![
                    MenuItem::action("System", UseSystemAppearance),
                    MenuItem::action("Light", UseLightAppearance),
                    MenuItem::action("Dark", UseDarkAppearance),
                ],
            })],
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
