//! Sign-in screen: device flow by default, plus signing in as yourself with
//! the GitHub CLI or a personal access token, for orgs that haven't approved
//! Docket.

use gpui::{
    AnyElement, App, Entity, Focusable as _, FontWeight, SharedString, Window, div, prelude::*, px,
};

use super::components::{
    Badge, Button, ButtonSize, ButtonStyle, Callout, CalloutKind, Icon, IconName, IconSize,
    MONOSPACE, Spinner,
};
use super::text_input::TextInput;
use super::theme::{Color, Theme};
use crate::config::{
    FINE_GRAINED_ORGANIZATION_PERMISSIONS, FINE_GRAINED_REPOSITORY_PERMISSIONS, GITHUB_WEB_URL,
    OAUTH_SCOPES,
};
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
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let wide = window.viewport_size().width >= px(TWO_COLUMN_MIN_WIDTH);

        let body = match &self.state.read(cx).auth {
            AuthStatus::Restoring | AuthStatus::SignedIn { .. } => progress("Loading…", theme),
            AuthStatus::Verifying => progress("Signing in…", theme),
            AuthStatus::SignedOut => self.options(theme).into_any_element(),
            AuthStatus::Failed(message) => div()
                .w(px(CARD_WIDTH))
                .flex()
                .flex_col()
                .gap_4()
                .child(Callout::new(CalloutKind::Error, message.clone()).title("Couldn't sign in"))
                .child(self.options(theme))
                .into_any_element(),
            AuthStatus::RequestingCode => progress("Contacting GitHub…", theme),
            AuthStatus::AwaitingUser(code) => {
                let uri = code.verification_uri.clone();
                let user_code = code.user_code.clone();
                div()
                    .w(px(CARD_WIDTH))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child("Enter this code on GitHub"),
                    )
                    .child(
                        div()
                            .w_full()
                            .py_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.elevated)
                            .text_center()
                            .text_3xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(user_code.clone()),
                    )
                    .child(
                        Button::new("open-github", "Copy code & open GitHub")
                            .icon(IconName::ExternalLink)
                            .full_width()
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    user_code.clone(),
                                ));
                                cx.open_url(&uri);
                            }),
                    )
                    .child(progress("Waiting for authorization…", theme))
                    .child(self.back_button())
                    .into_any_element()
            }
            AuthStatus::EnteringToken { error } => self.token_form(error.clone(), wide, theme),
        };

        // Centered when it fits; the token form can outgrow a short window.
        // Auto margins, unlike `justify_center`, never push content above
        // the top where it can't be scrolled to.
        let content = div()
            .flex_none()
            .my_auto()
            .flex()
            .flex_col()
            .items_center()
            .gap_8()
            .py_8()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size_12()
                            .rounded_xl()
                            .bg(theme.accent_subtle)
                            .child(
                                Icon::new(IconName::GitPullRequest)
                                    .size(IconSize::Large)
                                    .color(Color::Accent),
                            ),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Docket"),
                    )
                    .child(
                        div()
                            .text_color(theme.text_muted)
                            .child("Your pull requests, in one place."),
                    ),
            )
            .child(body);

        div()
            .id("sign-in")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(content)
    }
}

const CARD_WIDTH: f32 = 360.;
/// Window width from which the token form puts its guidance beside the
/// field rather than below it.
const TWO_COLUMN_MIN_WIDTH: f32 = 900.;

fn progress(label: &'static str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_color(theme.text_muted)
        .child(Spinner::new(label))
        .child(label)
        .into_any_element()
}

/// A labeled rule between groups of options.
fn divider(label: &'static str, theme: &Theme) -> impl IntoElement {
    let line = || div().flex_1().h_px().bg(theme.border_subtle);
    div()
        .w_full()
        .flex()
        .items_center()
        .gap_3()
        .text_xs()
        .text_color(theme.text_subtle)
        .child(line())
        .child(label)
        .child(line())
}

impl SignIn {
    fn options(&self, theme: &Theme) -> impl IntoElement + use<> {
        let device_flow = self.state.clone();
        let github_cli = self.state.clone();
        let token = self.state.clone();
        let token_focus = self.token_input.clone();

        div()
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .items_center()
            .gap_4()
            .child(
                Button::new("sign-in", "Sign in with GitHub")
                    .icon(IconName::LogIn)
                    .full_width()
                    .on_click(move |_, _, cx| {
                        device_flow.update(cx, |state, cx| state.start_sign_in(cx));
                    }),
            )
            .child(divider("or sign in as yourself", theme))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        Button::new("sign-in-gh", "Use GitHub CLI login")
                            .icon(IconName::Terminal)
                            .full_width()
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, _, cx| {
                                github_cli
                                    .update(cx, |state, cx| state.sign_in_with_github_cli(cx));
                            }),
                    )
                    .child(
                        Button::new("sign-in-token", "Use a personal access token")
                            .icon(IconName::KeyRound)
                            .full_width()
                            .style(ButtonStyle::Secondary)
                            .on_click(move |_, window, cx| {
                                token.update(cx, |state, cx| state.show_token_form(cx));
                                window.focus(&token_focus.focus_handle(cx));
                            }),
                    ),
            )
            .child(
                div()
                    .text_center()
                    .text_xs()
                    .text_color(theme.text_subtle)
                    .child(
                        "If your organization hasn't approved Docket, these use your own \
                         access instead of Docket's.",
                    ),
            )
    }

    fn token_form(&self, error: Option<SharedString>, wide: bool, theme: &Theme) -> AnyElement {
        let classic_url = format!(
            "{GITHUB_WEB_URL}/settings/tokens/new?description=Docket&scopes={}",
            OAUTH_SCOPES.join(",")
        );
        let fine_grained_url = format!("{GITHUB_WEB_URL}/settings/personal-access-tokens/new");
        let submit_state = self.state.clone();
        let submit_input = self.token_input.clone();

        let classic = token_kind("Classic token", true, theme)
            .child(requirement("Scopes", OAUTH_SCOPES, theme))
            .child(
                Button::new("create-classic-token", "Create a classic token…")
                    .icon(IconName::ExternalLink)
                    .style(ButtonStyle::Secondary)
                    .size(ButtonSize::Compact)
                    .on_click(move |_, _, cx| cx.open_url(&classic_url)),
            );
        let fine_grained = token_kind("Fine-grained token", false, theme)
            .child(note(
                "Covers a single account or organization, so Docket won't see pull \
                 requests anywhere else. Give it access to all of that owner's repositories.",
                theme,
            ))
            .child(requirement(
                "Repository permissions, read-only",
                FINE_GRAINED_REPOSITORY_PERMISSIONS,
                theme,
            ))
            .child(requirement(
                "Organization permissions, read-only",
                FINE_GRAINED_ORGANIZATION_PERMISSIONS,
                theme,
            ))
            .child(note(
                "GitHub doesn't let fine-grained tokens read check runs, so CI results \
                 from GitHub Actions won't show.",
                theme,
            ))
            .child(
                Button::new("create-fine-grained-token", "Create a fine-grained token…")
                    .icon(IconName::ExternalLink)
                    .style(ButtonStyle::Secondary)
                    .size(ButtonSize::Compact)
                    .on_click(move |_, _, cx| cx.open_url(&fine_grained_url)),
            );

        let field = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Personal access token"),
            )
            .child(self.token_input.clone())
            .children(error.map(|error| Callout::new(CalloutKind::Error, error)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(self.back_button())
                    .child(
                        Button::new("submit-token", "Sign in").on_click(move |_, _, cx| {
                            submit_token(&submit_state, &submit_input, cx)
                        }),
                    ),
            );
        let guidance = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Which permissions to grant"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.elevated)
                    .child(classic)
                    .child(div().h_px().bg(theme.border_subtle))
                    .child(fine_grained),
            )
            .child(note(
                "If your organization uses SAML single sign-on, choose Configure SSO next to \
                 the token on GitHub and authorize it. Organizations can also require \
                 approval before a fine-grained token works. Docket stores the token in your \
                 keychain.",
                theme,
            ));

        if wide {
            div()
                .flex()
                .items_start()
                .gap_8()
                .child(field.w(px(CARD_WIDTH)))
                .child(guidance.w(px(440.)))
        } else {
            div()
                .w(px(440.))
                .flex()
                .flex_col()
                .gap_5()
                .child(field)
                .child(guidance)
        }
        .into_any_element()
    }

    fn back_button(&self) -> Button {
        let state = self.state.clone();
        let token_input = self.token_input.clone();
        Button::new("sign-in-back", "Back")
            .icon(IconName::ArrowLeft)
            .style(ButtonStyle::Ghost)
            .on_click(move |_, _, cx| {
                token_input.update(cx, |input, cx| input.take(cx));
                state.update(cx, |state, cx| state.show_sign_in_options(cx));
            })
    }
}

/// One kind of personal access token and what it needs.
fn token_kind(title: &'static str, recommended: bool, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_start()
        .gap_2()
        .p_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().font_weight(FontWeight::MEDIUM).child(title))
                .when(recommended, |el| {
                    el.child(Badge::new("Recommended").color(Color::Accent))
                }),
        )
        .text_color(theme.text)
}

/// A label and the scopes or permissions it lists, as they appear on GitHub.
fn requirement(label: &'static str, items: &[&str], theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(theme.text_muted).child(label))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(items.iter().map(|item| {
                    div()
                        .px_1p5()
                        .rounded_md()
                        .bg(theme.element_selected)
                        .font_family(MONOSPACE)
                        .text_xs()
                        .child(item.to_string())
                })),
        )
}

fn note(text: &'static str, theme: &Theme) -> impl IntoElement {
    div().text_xs().text_color(theme.text_muted).child(text)
}
