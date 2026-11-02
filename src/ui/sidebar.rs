//! Left navigation: signed-in user and PR inbox sections.

use gpui::{App, Entity, Window, div, img, prelude::*, px};

use super::theme::Theme;
use crate::state::{AppState, AuthStatus, SectionKind};

#[derive(IntoElement)]
pub struct Sidebar {
    state: Entity<AppState>,
}

impl Sidebar {
    pub fn new(state: Entity<AppState>) -> Self {
        Self { state }
    }
}

impl RenderOnce for Sidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let state = self.state.read(cx);

        let user = match &state.auth {
            AuthStatus::SignedIn { user, .. } => Some(user),
            _ => None,
        };

        div()
            .w(px(240.))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.border)
            .children(user.map(|user| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(img(user.avatar_url.clone()).size_6().rounded_full())
                    .child(user.name.clone().unwrap_or_else(|| user.login.clone()))
            }))
            .child(div().flex().flex_col().p_2().gap_1().children(
                SectionKind::ALL.into_iter().map(|kind| {
                    let section = state.section(kind);
                    let selected = state.selected == kind;
                    let entity = self.state.clone();
                    div()
                        .id(kind.label())
                        .flex()
                        .justify_between()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(selected, |el| el.bg(theme.surface_hover))
                        .hover(|style| style.bg(theme.surface_hover))
                        .child(kind.label())
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .child(section.pull_requests.len().to_string()),
                        )
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |state, cx| state.select(kind, cx));
                        })
                }),
            ))
    }
}
