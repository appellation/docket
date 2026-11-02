mod auth;
mod config;
mod github;
mod projects;
mod state;
mod ui;

use gpui::{
    App, AppContext as _, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBounds, WindowOptions, actions, px, size,
};

use crate::config::Config;
use crate::state::AppState;
use crate::ui::{Workspace, text_input::TextInput, theme::Theme};

actions!(
    docket,
    [
        Quit,
        Refresh,
        SignOut,
        AddProject,
        NextProject,
        PreviousProject,
        Cancel
    ]
);

fn main() {
    let config = Config::load();

    Application::new().run(move |cx: &mut App| {
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
            |_window, cx| cx.new(|cx| Workspace::new(state, cx)),
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

    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-o", AddProject, None),
        KeyBinding::new("cmd-}", NextProject, None),
        KeyBinding::new("cmd-{", PreviousProject, None),
        KeyBinding::new("escape", Cancel, None),
    ]);
    cx.set_menus(vec![Menu {
        name: "Docket".into(),
        items: vec![
            MenuItem::action("Add Project…", AddProject),
            MenuItem::action("Refresh", Refresh),
            MenuItem::action("Sign Out", SignOut),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ],
    }]);
}
