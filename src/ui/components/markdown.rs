//! Renders a parsed Markdown `Document`. Links open in the browser.

use std::sync::Arc;

use gpui::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveText,
    StrikethroughStyle, StyledText, Window, div, prelude::*, px,
};

use super::MONOSPACE;
use super::icon::{Icon, IconName};
use crate::markdown::{Alert, Block, Document, Inline, InlineStyle};
use crate::ui::theme::{Color, Theme};

#[derive(IntoElement)]
pub struct Markdown {
    id: ElementId,
    doc: Arc<Document>,
}

impl Markdown {
    /// `id` scopes the ids of links and code blocks inside.
    pub fn new(id: impl Into<ElementId>, doc: Arc<Document>) -> Self {
        Self { id: id.into(), doc }
    }
}

impl RenderOnce for Markdown {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let mut ids = 0;
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_2()
            .min_w_0()
            .children(blocks(&self.doc.blocks, theme, &mut ids))
    }
}

fn blocks(blocks: &[Block], theme: &Theme, ids: &mut usize) -> Vec<AnyElement> {
    blocks.iter().map(|b| block(b, theme, ids)).collect()
}

fn block(block: &Block, theme: &Theme, ids: &mut usize) -> AnyElement {
    match block {
        Block::Paragraph(content) => div().child(text(content, theme, ids)).into_any_element(),
        Block::Heading { level, content } => {
            let heading = div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(text(content, theme, ids));
            match level {
                1 => heading
                    .text_lg()
                    .pb_1()
                    .border_b_1()
                    .border_color(theme.border_subtle),
                2 => heading.text_base(),
                _ => heading,
            }
            .into_any_element()
        }
        Block::Code(code) => {
            *ids += 1;
            let highlights = code
                .highlights
                .iter()
                .map(|(range, id)| (range.clone(), theme.syntax.style(*id)));
            let code = StyledText::new(code.text.clone()).with_highlights(highlights);
            div()
                .id(("code", *ids))
                .overflow_x_scroll()
                .rounded_md()
                .border_1()
                .border_color(theme.border_subtle)
                .bg(theme.surface)
                .px_3()
                .py_2()
                .font_family(MONOSPACE)
                .text_xs()
                .child(div().whitespace_nowrap().child(code))
                .into_any_element()
        }
        Block::Quote {
            alert,
            blocks: inner,
        } => {
            let alert = alert.map(|alert| match alert {
                Alert::Note => ("Note", IconName::Info, Color::Accent),
                Alert::Tip => ("Tip", IconName::Lightbulb, Color::Success),
                Alert::Important => ("Important", IconName::MessageSquareWarning, Color::Merged),
                Alert::Warning => ("Warning", IconName::TriangleAlert, Color::Warning),
                Alert::Caution => ("Caution", IconName::OctagonAlert, Color::Danger),
            });
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pl_3()
                .border_l_2()
                .map(|el| match alert {
                    Some((title, icon, color)) => el.border_color(color.hsla(theme)).child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color.hsla(theme))
                            .child(Icon::new(icon).color(color))
                            .child(title),
                    ),
                    None => el.border_color(theme.border).text_color(theme.text_muted),
                })
                .children(blocks(inner, theme, ids))
                .into_any_element()
        }
        Block::List { start, items } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(items.iter().enumerate().map(|(ix, item)| {
                let marker = match (item.checked, start) {
                    (Some(checked), _) => div().h(px(22.)).flex().items_center().child(
                        Icon::new(if checked {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        })
                        .color(if checked {
                            Color::Accent
                        } else {
                            Color::Subtle
                        }),
                    ),
                    (None, Some(start)) => div()
                        .text_color(theme.text_muted)
                        .child(format!("{}.", start + ix as u64)),
                    (None, None) => div().text_color(theme.text_subtle).child("•"),
                };
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(14.))
                            .flex()
                            .justify_end()
                            .child(marker),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .children(blocks(&item.blocks, theme, ids)),
                    )
            }))
            .into_any_element(),
        Block::Table { header, rows } => {
            let columns = rows
                .iter()
                .map(Vec::len)
                .chain([header.len()])
                .max()
                .unwrap_or(0);
            let row = |cells: &[Inline], is_header: bool, ids: &mut usize| {
                div()
                    .flex()
                    .when(!is_header, |el| el.border_t_1().border_color(theme.border))
                    .when(is_header, |el| {
                        el.bg(theme.surface).font_weight(FontWeight::SEMIBOLD)
                    })
                    .children((0..columns).map(|ix| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .when(ix > 0, |el| el.border_l_1().border_color(theme.border))
                            .children(cells.get(ix).map(|cell| text(cell, theme, ids)))
                    }))
            };
            let header = row(header, true, ids);
            let rows: Vec<_> = rows.iter().map(|cells| row(cells, false, ids)).collect();
            div()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .text_xs()
                .child(header)
                .children(rows)
                .into_any_element()
        }
        Block::Rule => div().h(px(1.)).bg(theme.border).into_any_element(),
    }
}

fn text(content: &Inline, theme: &Theme, ids: &mut usize) -> AnyElement {
    let highlights = content
        .runs
        .iter()
        .map(|(range, style)| (range.clone(), highlight(*style, theme)));
    let styled = StyledText::new(content.text.clone()).with_highlights(highlights);
    if content.links.is_empty() {
        return styled.into_any_element();
    }
    *ids += 1;
    let (ranges, urls): (Vec<_>, Vec<_>) = content.links.iter().cloned().unzip();
    InteractiveText::new(("text", *ids), styled)
        .on_click(ranges, move |ix, _, cx| cx.open_url(&urls[ix]))
        .into_any_element()
}

fn highlight(style: InlineStyle, theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: style.link.then_some(theme.text_accent),
        font_weight: style.bold.then_some(FontWeight::SEMIBOLD),
        font_style: style.italic.then_some(FontStyle::Italic),
        // Highlights can't change the font, so inline code is marked by its
        // background alone.
        background_color: style.code.then_some(theme.element_selected),
        strikethrough: style.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
        ..Default::default()
    }
}
