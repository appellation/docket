//! Device-flow sign-in screen.

use gpui::{App, ClickEvent, Entity, SharedString, Window, div, prelude::*};

use super::theme::Theme;
use crate::state::{AppState, AuthStatus};

#[derive(IntoElement)]
pub struct SignIn {
    state: Entity<AppState>,
}

impl SignIn {
    pub fn new(state: Entity<AppState>) -> Self {
        Self { state }
    }
}

fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let theme = Theme::get(cx);
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_md()
        .bg(theme.accent)
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .child(label.into())
        .on_click(on_click)
}

impl RenderOnce for SignIn {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let state = self.state.clone();
        let start = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            state.update(cx, |state, cx| state.start_sign_in(cx));
        };

        let body = match &self.state.read(cx).auth {
            AuthStatus::Restoring | AuthStatus::SignedIn { .. } => {
                div().child("Loading…").into_any_element()
            }
            AuthStatus::SignedOut => {
                button("sign-in", "Sign in with GitHub", cx, start).into_any_element()
            }
            AuthStatus::RequestingCode => div().child("Contacting GitHub…").into_any_element(),
            AuthStatus::AwaitingUser(code) => {
                let uri = code.verification_uri.clone();
                let user_code = code.user_code.clone();
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child("Enter this code on GitHub:"),
                    )
                    .child(
                        div()
                            .text_3xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(user_code.clone()),
                    )
                    .child(button(
                        "open-github",
                        "Copy code & open GitHub",
                        cx,
                        move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                user_code.clone(),
                            ));
                            cx.open_url(&uri);
                        },
                    ))
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child("Waiting for authorization…"),
                    )
                    .into_any_element()
            }
            AuthStatus::Failed(message) => div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(div().text_color(theme.danger).child(message.clone()))
                .child(button("retry", "Try again", cx, start))
                .into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .child(div().text_xl().child("Docket"))
            .child(body)
    }
}
