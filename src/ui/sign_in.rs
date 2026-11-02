//! Sign-in screen: device flow by default, plus signing in as yourself with
//! the GitHub CLI or a personal access token, for orgs that haven't approved
//! Docket.

use gpui::{AnyElement, App, Entity, Focusable as _, SharedString, Window, div, prelude::*, px};

use super::components::{Button, ButtonStyle};
use super::text_input::TextInput;
use super::theme::Theme;
use crate::config::{GITHUB_WEB_URL, OAUTH_SCOPES};
use crate::state::{AppState, AuthStatus};

#[derive(IntoElement)]
pub struct SignIn {
    state: Entity<AppState>,
    token_input: Entity<TextInput>,
}

impl SignIn {
    pub fn new(state: Entity<AppState>, token_input: Entity<TextInput>) -> Self {
        Self { state, token_input }
    }
}

/// Takes the token out of the field (so it isn't kept around) and signs in.
pub fn submit_token(state: &Entity<AppState>, token_input: &Entity<TextInput>, cx: &mut App) {
    let token = token_input.update(cx, |input, cx| input.take(cx));
    state.update(cx, |state, cx| state.sign_in_with_token(token, cx));
}

impl RenderOnce for SignIn {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);

        let body = match &self.state.read(cx).auth {
            AuthStatus::Restoring | AuthStatus::SignedIn { .. } => {
                div().child("Loading…").into_any_element()
            }
            AuthStatus::Verifying => div().child("Signing in…").into_any_element(),
            AuthStatus::SignedOut => self.options(theme).into_any_element(),
            AuthStatus::Failed(message) => div()
                .flex()
                .flex_col()
                .items_center()
                .gap_6()
                .child(
                    div()
                        .max_w(px(480.))
                        .text_center()
                        .text_color(theme.danger)
                        .child(message.clone()),
                )
                .child(self.options(theme))
                .into_any_element(),
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
                    .child(
                        Button::new("open-github", "Copy code & open GitHub").on_click(
                            move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    user_code.clone(),
                                ));
                                cx.open_url(&uri);
                            },
                        ),
                    )
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child("Waiting for authorization…"),
                    )
                    .child(self.back_button())
                    .into_any_element()
            }
            AuthStatus::EnteringToken { error } => self.token_form(error.clone(), theme),
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

impl SignIn {
    fn options(&self, theme: &Theme) -> impl IntoElement + use<> {
        let device_flow = self.state.clone();
        let github_cli = self.state.clone();
        let token = self.state.clone();
        let token_focus = self.token_input.clone();

        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_4()
            .child(
                Button::new("sign-in", "Sign in with GitHub").on_click(move |_, _, cx| {
                    device_flow.update(cx, |state, cx| state.start_sign_in(cx));
                }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.text_muted)
                    .child("or sign in as yourself"),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("sign-in-gh", "Use GitHub CLI login")
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, _, cx| {
                                github_cli
                                    .update(cx, |state, cx| state.sign_in_with_github_cli(cx));
                            }),
                    )
                    .child(
                        Button::new("sign-in-token", "Use a personal access token")
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, window, cx| {
                                token.update(cx, |state, cx| state.show_token_form(cx));
                                window.focus(&token_focus.focus_handle(cx));
                            }),
                    ),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_center()
                    .text_xs()
                    .text_color(theme.text_muted)
                    .child(
                        "If your organization hasn't approved Docket, these use your own \
                         access instead of Docket's.",
                    ),
            )
    }

    fn token_form(&self, error: Option<SharedString>, theme: &Theme) -> AnyElement {
        let create_url = format!(
            "{GITHUB_WEB_URL}/settings/tokens/new?description=Docket&scopes={}",
            OAUTH_SCOPES.join(",")
        );
        let submit_state = self.state.clone();
        let submit_input = self.token_input.clone();

        div()
            .w(px(440.))
            .flex()
            .flex_col()
            .gap_3()
            .child("Personal access token")
            .child(self.token_input.clone())
            .children(error.map(|error| div().text_xs().text_color(theme.danger).child(error)))
            .child(div().text_xs().text_color(theme.text_muted).child(format!(
                        "Classic tokens need the {} scopes. If your organization uses SAML \
                         single sign-on, authorize the token for it on GitHub (Configure SSO). \
                         The token is stored in your keychain.",
                        OAUTH_SCOPES
                            .iter()
                            .map(|s| format!("`{s}`"))
                            .collect::<Vec<_>>()
                            .join(" and ")
                    )))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(
                        Button::new("create-token", "Create a token…")
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, _, cx| cx.open_url(&create_url)),
                    )
                    .child(div().flex().gap_2().child(self.back_button()).child(
                        Button::new("submit-token", "Sign in").on_click(move |_, _, cx| {
                            submit_token(&submit_state, &submit_input, cx)
                        }),
                    )),
            )
            .into_any_element()
    }

    fn back_button(&self) -> Button {
        let state = self.state.clone();
        let token_input = self.token_input.clone();
        Button::new("sign-in-back", "Back")
            .style(ButtonStyle::Secondary)
            .on_click(move |_, _, cx| {
                token_input.update(cx, |input, cx| input.take(cx));
                state.update(cx, |state, cx| state.show_sign_in_options(cx));
            })
    }
}
