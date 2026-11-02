mod auth;
mod config;
mod diff;
mod github;
mod http_client;
mod projects;
mod state;
mod syntax;
mod ui;

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBounds, WindowOptions, actions, px, size,
};

use crate::config::Config;
use crate::http_client::UreqHttpClient;
use crate::state::AppState;
use crate::ui::theme::{Theme, ThemeMode};
use crate::ui::{Assets, Workspace, text_input::TextInput};

actions!(
    docket,
    [
        Quit,
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

fn main() {
    let config = Config::load();

    Application::new()
        .with_assets(Assets)
        .with_http_client(Arc::new(UreqHttpClient::new()))
        .run(move |cx: &mut App| {
            Theme::init(cx);
            TextInput::bind_keys(cx);
            let state = AppState::init(config, cx);
            register_actions(cx);

            let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Docket".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    window
                        .observe_window_appearance(|_, cx| Theme::sync_system_appearance(cx))
                        .detach();
                    cx.new(|cx| Workspace::new(state, cx))
                },
            )
            .expect("failed to open window");
            cx.activate(true);
        });
}

fn register_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Refresh, cx| {
        AppState::global(cx).update(cx, |state, cx| state.refresh(cx));
    });
    cx.on_action(|_: &SignOut, cx| {
        AppState::global(cx).update(cx, |state, cx| state.sign_out(cx));
    });
    cx.on_action(|_: &AddProject, cx| {
        AppState::global(cx).update(cx, |state, cx| state.add_project(cx));
    });
    cx.on_action(|_: &NextProject, cx| {
        AppState::global(cx).update(cx, |state, cx| state.select_adjacent_project(1, cx));
    });
    cx.on_action(|_: &PreviousProject, cx| {
        AppState::global(cx).update(cx, |state, cx| state.select_adjacent_project(-1, cx));
    });
    cx.on_action(|_: &Cancel, cx| {
        AppState::global(cx).update(cx, |state, cx| state.cancel(cx));
    });
    cx.on_action(|_: &UseSystemAppearance, cx| Theme::set_mode(ThemeMode::System, cx));
    cx.on_action(|_: &UseLightAppearance, cx| Theme::set_mode(ThemeMode::Light, cx));
    cx.on_action(|_: &UseDarkAppearance, cx| Theme::set_mode(ThemeMode::Dark, cx));

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
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
    ]);
}
