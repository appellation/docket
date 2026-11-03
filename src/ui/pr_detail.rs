//! The open pull request: its conversation, loosely after GitHub's (the
//! description, the timeline, then where it stands on reviews, checks, and
//! merging, with reviewers and labels alongside), and its files.
//!
//! Bookkeeping (review requests, deployments, labels, force-pushes…) and bot
//! comments are collapsed so the discussion stands out. Bot reviews are not:
//! they're feedback on the code.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, Entity, FontWeight, HighlightStyle, SharedString, StyledText, Window, div,
    img, prelude::*, px, rgb,
};

use super::commit_list::CommitList;
use super::components::{
    Badge, Button, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName, IconSize,
    MONOSPACE, Markdown, Spinner, TabBar, TabBarItem, Tooltip, relative_time,
};
use super::diff_view::DiffView;
use super::theme::{Color, Theme};
use crate::github::{
    Actor, Check, CheckStatus, Comment, Commit, Event, EventKind, MergeStatus, PullRequestDetail,
    PullRequestState, Review, ReviewDecision, ReviewState, ReviewThread, Reviewer, ReviewerStatus,
    TimelineItem,
};
use crate::state::{LoadedDetail, PrDetail, PrTab, Tab};

/// Width of the column holding avatars and event icons, which the
/// timeline's connecting line runs down the middle of.
const RAIL_WIDTH: f32 = 40.;
/// Lines of a review thread's diff hunk to show above its comments.
const HUNK_CONTEXT_LINES: usize = 4;

#[derive(IntoElement)]
pub struct PrDetailView {
    state: Entity<Tab>,
    /// The Files tab. It keeps scroll position and collapsed files, so it's
    /// an entity owned by the workspace rather than rebuilt each frame.
    diff_view: Option<Entity<DiffView>>,
}

impl PrDetailView {
    pub fn new(state: Entity<Tab>, diff_view: Option<Entity<DiffView>>) -> Self {
        Self { state, diff_view }
    }
}

impl RenderOnce for PrDetailView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::get(cx);
        let root = div().flex_1().min_w_0().h_full().flex().flex_col();
        let Some(detail) = &self.state.read(cx).detail else {
            return root;
        };
        let header = header(detail, &self.state, theme);
        let tabs = tabs(detail, &self.state);
        if detail.tab == PrTab::Files {
            return root.child(header).child(tabs).children(self.diff_view);
        }

        let body = match &detail.loaded {
            Some(loaded) if detail.tab == PrTab::Commits => commits_tab(loaded).into_any_element(),
            Some(loaded) => {
                let view = View {
                    theme,
                    state: &self.state,
                    detail,
                    loaded,
                };
                div()
                    .id(SharedString::from(format!("pr-detail-{}", loaded.pr.id)))
                    .flex_1()
                    .overflow_y_scroll()
                    .child(view.title())
                    .children(detail.error.clone().map(|err| {
                        div().px_6().pt_4().child(
                            Callout::new(CalloutKind::Error, err)
                                .title("Couldn't refresh this pull request"),
                        )
                    }))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_6()
                            .max_w(px(1120.))
                            .px_6()
                            .py_4()
                            .child(view.timeline())
                            .child(view.side_panel()),
                    )
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .flex()
                .flex_col()
                .children(summary_title(detail, theme))
                .child(match &detail.error {
                    Some(err) => div()
                        .p_6()
                        .child(
                            Callout::new(CalloutKind::Error, err.clone())
                                .title("Couldn't load this pull request"),
                        )
                        .into_any_element(),
                    None => div()
                        .flex()
                        .justify_center()
                        .pt(px(64.))
                        .child(Spinner::new("pr-detail-loading"))
                        .into_any_element(),
                })
                .into_any_element(),
        };

        root.child(header).child(tabs).child(body)
    }
}

fn tabs(detail: &PrDetail, state: &Entity<Tab>) -> impl IntoElement {
    let pr = detail.loaded.as_ref().map(|loaded| &loaded.pr);
    let commits = pr.map(|pr| pr.commit_count.to_string());
    let files = pr.map(|pr| pr.changed_files.to_string());
    let tab = |tab: PrTab, id: &'static str, icon: IconName, label: &'static str| {
        let state = state.clone();
        TabBarItem::new(id, icon, label)
            .selected(detail.tab == tab)
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| state.select_pr_tab(tab, cx));
            })
    };
    TabBar::new()
        .child(tab(
            PrTab::Conversation,
            "pr-tab-conversation",
            IconName::MessageSquare,
            "Conversation",
        ))
        .child(
            tab(
                PrTab::Commits,
                "pr-tab-commits",
                IconName::GitCommit,
                "Commits",
            )
            .count(commits),
        )
        .child(tab(PrTab::Files, "pr-tab-files", IconName::FileDiff, "Files").count(files))
}

/// The Commits tab: those in the timeline, which are the pull request's
/// commits as of its head. A timeline too long to load in full may be
/// missing some, so it says so.
fn commits_tab(loaded: &LoadedDetail) -> impl IntoElement {
    let pr = &loaded.pr;
    let commits: Vec<Commit> = pr
        .timeline
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Commit(commit) => Some(commit.clone()),
            _ => None,
        })
        .collect();
    let missing = (commits.len() as u64) < pr.commit_count;
    let notice = missing.then(|| {
        let url = format!("{}/commits", pr.url);
        Callout::new(
            CalloutKind::Warning,
            format!("Showing {} of {} commits.", commits.len(), pr.commit_count),
        )
        .action(
            Button::new("pr-commits-github", "View all on GitHub")
                .icon(IconName::ExternalLink)
                .style(ButtonStyle::Secondary)
                .on_click(move |_, _, cx| cx.open_url(&url)),
        )
    });
    CommitList::new(SharedString::from(format!("pr-commits-{}", pr.id)), commits).notice(notice)
}

fn header(detail: &PrDetail, state: &Entity<Tab>, theme: &Theme) -> impl IntoElement {
    let url = detail.url();
    let (back, refresh) = (state.clone(), state.clone());
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .gap_4()
        .h(px(48.))
        .pl_2()
        .pr_4()
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    IconButton::new("pr-back", IconName::ArrowLeft)
                        .tooltip("Back (Esc)")
                        .on_click(move |_, _, cx| {
                            back.update(cx, |state, cx| state.close_pull_request(cx));
                        }),
                )
                .child(
                    div()
                        .truncate()
                        .text_color(theme.text_muted)
                        .child(format!("{}#{}", detail.repo, detail.number)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .when(
                    (detail.loading && detail.loaded.is_some())
                        || detail.files.as_ref().is_some_and(|files| files.is_busy()),
                    |el| el.child(Spinner::new("pr-detail-refreshing")),
                )
                .child(
                    Button::new("pr-open-github", "Open on GitHub")
                        .icon(IconName::ExternalLink)
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
                .child(
                    IconButton::new("pr-refresh", IconName::RefreshCw)
                        .tooltip("Refresh (⌘R)")
                        .disabled(detail.loading)
                        .on_click(move |_, _, cx| {
                            refresh.update(cx, |state, cx| state.refresh(cx));
                        }),
                ),
        )
}

/// The title and what's known from the list row, while the detail loads.
fn summary_title(detail: &PrDetail, theme: &Theme) -> Option<impl IntoElement> {
    let pr = detail.summary.as_ref()?;
    Some(title_block(
        &pr.title,
        pr.number,
        state_badge(pr.state, pr.is_draft),
        div()
            .text_color(theme.text_muted)
            .children(pr.author.clone().map(|a| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(a)
            }))
            .into_any_element(),
        theme,
    ))
}

fn title_block(
    title: &str,
    number: u64,
    badge: Badge,
    meta: AnyElement,
    theme: &Theme,
) -> impl IntoElement {
    let number = format!(" #{number}");
    let text = format!("{title}{number}");
    let number_range = title.len()..text.len();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px_6()
        .pt_5()
        .pb_4()
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(
            StyledText::new(text).with_highlights([(
                number_range,
                HighlightStyle {
                    color: Some(theme.text_subtle),
                    font_weight: Some(FontWeight::NORMAL),
                    ..Default::default()
                },
            )]),
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_x_2()
                .gap_y_1()
                .child(badge)
                .child(div().flex_1().min_w_0().child(meta)),
        )
}

fn state_badge(state: PullRequestState, is_draft: bool) -> Badge {
    match (state, is_draft) {
        (PullRequestState::Merged, _) => Badge::new("Merged")
            .icon(IconName::GitMerge)
            .color(Color::Merged),
        (PullRequestState::Closed, _) => Badge::new("Closed")
            .icon(IconName::GitPullRequestClosed)
            .color(Color::Danger),
        (_, true) => Badge::new("Draft").icon(IconName::GitPullRequestDraft),
        (_, false) => Badge::new("Open")
            .icon(IconName::GitPullRequest)
            .color(Color::Success),
    }
}

/// What the timeline shows, after collapsing.
#[derive(Debug)]
enum Entry<'a> {
    Comment(&'a Comment),
    Review(&'a Review),
    /// Consecutive commits.
    Commits(Vec<&'a Commit>),
    /// An event that changed where the pull request stands.
    Event(&'a Event),
    /// Consecutive bookkeeping events, collapsed together.
    Events(Vec<&'a Event>),
}

fn group_timeline(items: &[TimelineItem]) -> Vec<Entry<'_>> {
    let mut entries = Vec::new();
    for item in items {
        match item {
            TimelineItem::Comment(comment) => entries.push(Entry::Comment(comment)),
            // A bare comment review is just replies to other reviews'
            // threads, which show in those threads.
            TimelineItem::Review(review)
                if review.state == ReviewState::Commented
                    && review.threads.is_empty()
                    && review.body.trim().is_empty() => {}
            TimelineItem::Review(review) => entries.push(Entry::Review(review)),
            TimelineItem::Commit(commit) => match entries.last_mut() {
                Some(Entry::Commits(commits)) => commits.push(commit),
                _ => entries.push(Entry::Commits(vec![commit])),
            },
            // Merging closes the pull request too; GitHub only shows the merge.
            TimelineItem::Event(Event {
                kind: EventKind::Closed,
                ..
            }) if matches!(
                entries.last(),
                Some(Entry::Event(Event {
                    kind: EventKind::Merged { .. },
                    ..
                }))
            ) => {}
            TimelineItem::Event(event) if event.kind.is_notable() => {
                entries.push(Entry::Event(event))
            }
            TimelineItem::Event(event) => match entries.last_mut() {
                Some(Entry::Events(events)) => events.push(event),
                _ => entries.push(Entry::Events(vec![event])),
            },
        }
    }
    entries
}

/// "review requests, deployments": what a run of collapsed events is made
/// of, in the order they first appear.
fn summarize_events(events: &[&Event]) -> String {
    let mut categories: Vec<&str> = Vec::new();
    for event in events {
        let category = event.kind.category();
        if !categories.contains(&category) {
            categories.push(category);
        }
    }
    categories.join(", ")
}

/// Everything rendering the loaded detail needs.
struct View<'a> {
    theme: &'a Theme,
    state: &'a Entity<Tab>,
    detail: &'a PrDetail,
    loaded: &'a LoadedDetail,
}

impl View<'_> {
    fn pr(&self) -> &PullRequestDetail {
        &self.loaded.pr
    }

    fn is_author(&self, actor: Option<&Actor>) -> bool {
        let author = self.pr().author.as_ref().map(|a| a.login.as_str());
        actor.is_some_and(|a| Some(a.login.as_str()) == author)
    }

    fn toggle(
        &self,
        key: SharedString,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + use<> {
        let state = self.state.clone();
        move |_, _, cx| {
            let key = key.clone();
            state.update(cx, |state, cx| state.toggle_detail_entry(key, cx));
        }
    }

    // --- Title ------------------------------------------------------------

    fn title(&self) -> impl IntoElement {
        let pr = self.pr();
        let theme = self.theme;
        let commits = match pr.commit_count {
            1 => "1 commit".to_owned(),
            n => format!("{n} commits"),
        };
        let action = match pr.state {
            PullRequestState::Merged => format!("merged {commits} into"),
            _ => format!("wants to merge {commits} into"),
        };
        let files = match pr.changed_files {
            1 => "1 file".to_owned(),
            n => format!("{n} files"),
        };
        let meta = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_1p5()
            .gap_y_1()
            .text_color(theme.text_muted)
            .children(pr.author.as_ref().map(|author| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(author.login.clone())
            }))
            .child(action)
            .child(branch(&pr.base_ref, theme))
            .child("from")
            .child(branch(&pr.head_ref, theme))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .pl_2()
                    .text_xs()
                    .child(
                        div()
                            .text_color(theme.success)
                            .child(format!("+{}", pr.additions)),
                    )
                    .child(
                        div()
                            .text_color(theme.danger)
                            .child(format!("−{}", pr.deletions)),
                    )
                    .child(
                        div()
                            .text_color(theme.text_subtle)
                            .child(format!("· {files}")),
                    ),
            );
        title_block(
            &pr.title,
            pr.number,
            state_badge(pr.state, pr.is_draft),
            meta.into_any_element(),
            theme,
        )
    }

    // --- Timeline ---------------------------------------------------------

    fn timeline(&self) -> impl IntoElement {
        let pr = self.pr();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(self.description())
            .children(
                group_timeline(&pr.timeline)
                    .into_iter()
                    .map(|entry| self.entry(entry)),
            )
            .when(pr.state == PullRequestState::Open, |el| {
                el.child(self.merge_box())
            })
    }

    fn entry(&self, entry: Entry) -> AnyElement {
        match entry {
            Entry::Comment(comment) => self.comment(comment),
            Entry::Review(review) => self.review(review),
            Entry::Commits(commits) => self.commits(&commits),
            Entry::Event(event) => self.event(event),
            Entry::Events(events) => self.events(&events),
        }
    }

    fn description(&self) -> AnyElement {
        let pr = self.pr();
        let doc = self.loaded.doc(&pr.id);
        let body = if doc.is_empty() {
            div()
                .italic()
                .text_color(self.theme.text_subtle)
                .child("No description provided.")
                .into_any_element()
        } else {
            Markdown::new(SharedString::from(pr.id.clone()), doc).into_any_element()
        };
        rail(
            avatar(pr.author.as_ref(), 28., self.theme),
            self.card(
                self.card_header(
                    &pr.id,
                    pr.author.as_ref(),
                    "opened this",
                    pr.created_at,
                    &pr.url,
                ),
                Some(body),
            ),
            self.theme,
        )
        .into_any_element()
    }

    fn comment(&self, comment: &Comment) -> AnyElement {
        let theme = self.theme;
        let collapsible = comment.is_by_bot() || comment.minimized.is_some();
        let key = SharedString::from(comment.id.clone());
        let expanded = self.detail.is_expanded(&key, !collapsible);

        if !expanded {
            let login = comment
                .author
                .as_ref()
                .map_or("ghost", |a| a.login.as_str());
            let summary = match &comment.minimized {
                Some(reason) if reason.is_empty() => {
                    format!("{login}'s comment was hidden")
                }
                Some(reason) => format!("{login}'s comment was hidden as {reason}"),
                None => String::new(),
            };
            let preview = self.loaded.doc(&comment.id).preview();
            let row = div()
                .flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .text_color(theme.text_muted)
                .map(|el| match comment.minimized {
                    Some(_) => el.child(div().flex_none().child(summary)),
                    None => el
                        .child(
                            div()
                                .flex_none()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(login.to_owned()),
                        )
                        .child(Badge::new("bot"))
                        .child(div().flex_none().child("commented")),
                })
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.text_subtle)
                        .child(relative_time(comment.created_at)),
                )
                .when(comment.minimized.is_none(), |el| {
                    el.children(preview.map(|preview| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_subtle)
                            .child(preview)
                    }))
                });
            return rail(
                avatar(comment.author.as_ref(), 24., theme),
                disclosure(key.clone(), false, row, theme).on_click(self.toggle(key)),
                theme,
            )
            .into_any_element();
        }

        let header = self
            .card_header(
                &comment.id,
                comment.author.as_ref(),
                "commented",
                comment.created_at,
                &comment.url,
            )
            .when(collapsible, |el| {
                el.cursor_pointer()
                    .on_click(self.toggle(key.clone()))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(IconSize::XSmall)
                            .color(Color::Subtle),
                    )
            });
        rail(
            avatar(comment.author.as_ref(), 28., theme),
            self.card(
                header,
                Some(Markdown::new(key, self.loaded.doc(&comment.id)).into_any_element()),
            ),
            theme,
        )
        .into_any_element()
    }

    fn review(&self, review: &Review) -> AnyElement {
        let theme = self.theme;
        let (verb, icon, color) = match review.state {
            ReviewState::Approved => (
                "approved these changes",
                IconName::CircleCheck,
                Color::Success,
            ),
            ReviewState::ChangesRequested => {
                ("requested changes", IconName::FileDiff, Color::Danger)
            }
            ReviewState::Dismissed => ("reviewed (dismissed)", IconName::CircleX, Color::Subtle),
            ReviewState::Commented | ReviewState::Pending => {
                ("reviewed", IconName::MessageSquare, Color::Muted)
            }
        };
        let doc = self.loaded.doc(&review.id);

        // Nothing to read: a one-line event, as GitHub shows it.
        if doc.is_empty() && review.threads.is_empty() {
            let login = review.author.as_ref().map(|a| a.login.as_str());
            return rail(
                event_icon(icon, color, theme),
                event_row(
                    sentence(login, verb, Some(review.submitted_at), theme),
                    theme,
                ),
                theme,
            )
            .into_any_element();
        }

        let header = self
            .card_header(
                &review.id,
                review.author.as_ref(),
                verb,
                review.submitted_at,
                &review.url,
            )
            .child(Icon::new(icon).color(color));
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .when(!doc.is_empty(), |el| {
                el.child(Markdown::new(SharedString::from(review.id.clone()), doc))
            })
            .children(review.threads.iter().map(|thread| self.thread(thread)));
        rail(
            avatar(review.author.as_ref(), 28., theme),
            self.card(header, Some(body.into_any_element())),
            theme,
        )
        .into_any_element()
    }

    fn thread(&self, thread: &ReviewThread) -> AnyElement {
        let theme = self.theme;
        let hover = theme.element_hover;
        let key = SharedString::from(thread.id.clone());
        // GitHub folds threads that no longer need attention.
        let expanded = self
            .detail
            .is_expanded(&key, !(thread.is_resolved || thread.is_outdated));

        let header =
            div()
                .id(key.clone())
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1()
                .bg(theme.surface)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .text_xs()
                .on_click(self.toggle(key.clone()))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(IconSize::XSmall)
                    .color(Color::Subtle),
                )
                .child(Icon::new(IconName::FileCode).size(IconSize::XSmall))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(MONOSPACE)
                        .child(match thread.line {
                            Some(line) => format!("{}:{line}", thread.path),
                            None => thread.path.clone(),
                        }),
                )
                .when(thread.is_outdated, |el| {
                    el.child(Badge::new("Outdated").color(Color::Warning))
                })
                .when(thread.is_resolved, |el| el.child(Badge::new("Resolved")))
                .when(!expanded, |el| {
                    el.child(div().text_color(theme.text_subtle).child(
                        match thread.comments.len() {
                            1 => "1 comment".to_owned(),
                            n => format!("{n} comments"),
                        },
                    ))
                });

        div()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(header)
            .when(expanded, |el| {
                el.child(diff_hunk(&thread.id, &thread.diff_hunk, theme))
                    .children(thread.comments.iter().map(|comment| {
                        div()
                            .flex()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .border_t_1()
                            .border_color(theme.border_subtle)
                            .child(avatar(comment.author.as_ref(), 20., theme))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .text_xs()
                                            .child(actor_name(comment.author.as_ref(), theme))
                                            .when(comment.is_by_bot(), |el| {
                                                el.child(Badge::new("bot"))
                                            })
                                            .child(
                                                div()
                                                    .text_color(theme.text_subtle)
                                                    .child(relative_time(comment.created_at)),
                                            ),
                                    )
                                    .child(Markdown::new(
                                        SharedString::from(comment.id.clone()),
                                        self.loaded.doc(&comment.id),
                                    )),
                            )
                    }))
            })
            .into_any_element()
    }

    fn commits(&self, commits: &[&Commit]) -> AnyElement {
        let theme = self.theme;
        let lead = event_icon(IconName::GitCommit, Color::Muted, theme);
        if let [commit] = commits {
            return rail(lead, event_row(commit_row(commit, theme), theme), theme)
                .into_any_element();
        }

        let key = SharedString::from(format!("commits-{}", commits[0].id));
        let expanded = self.detail.is_expanded(&key, false);
        let author = commits[0].author.as_deref();
        let same_author = commits.iter().all(|c| c.author.as_deref() == author);
        let summary = sentence(
            author.filter(|_| same_author),
            &format!("added {} commits", commits.len()),
            commits.last().map(|c| c.committed_at),
            theme,
        );
        rail(
            lead,
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(disclosure(key.clone(), expanded, summary, theme).on_click(self.toggle(key)))
                .when(expanded, |el| {
                    el.children(
                        commits
                            .iter()
                            .map(|commit| div().pl_5().child(commit_row(commit, theme))),
                    )
                }),
            theme,
        )
        .into_any_element()
    }

    fn event(&self, event: &Event) -> AnyElement {
        use EventKind as K;
        let theme = self.theme;
        let (icon, color) = match event.kind {
            K::Merged { .. } => (IconName::GitMerge, Color::Merged),
            K::Closed => (IconName::GitPullRequestClosed, Color::Danger),
            K::Reopened => (IconName::GitPullRequest, Color::Success),
            K::ReadyForReview => (IconName::Eye, Color::Muted),
            K::ConvertedToDraft => (IconName::GitPullRequestDraft, Color::Muted),
            _ => (IconName::Ellipsis, Color::Subtle),
        };
        rail(
            event_icon(icon, color, theme),
            event_row(event_sentence(event, theme), theme),
            theme,
        )
        .into_any_element()
    }

    fn events(&self, events: &[&Event]) -> AnyElement {
        let theme = self.theme;
        let lead = event_icon(IconName::Ellipsis, Color::Subtle, theme);
        if let [event] = events {
            return rail(lead, event_row(event_sentence(event, theme), theme), theme)
                .into_any_element();
        }

        let key = SharedString::from(format!("events-{}", events[0].id));
        let expanded = self.detail.is_expanded(&key, false);
        let count = format!("{} events", events.len());
        let summary = format!("{count} · {}", summarize_events(events));
        let summary = StyledText::new(summary).with_highlights([(
            0..count.len(),
            HighlightStyle {
                color: Some(theme.text),
                font_weight: Some(FontWeight::MEDIUM),
                ..Default::default()
            },
        )]);
        rail(
            lead,
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(disclosure(key.clone(), expanded, summary, theme).on_click(self.toggle(key)))
                .when(expanded, |el| {
                    el.children(events.iter().map(|event| {
                        div()
                            .pl_5()
                            .py_0p5()
                            .text_color(theme.text_muted)
                            .child(event_sentence(event, theme))
                    }))
                }),
            theme,
        )
        .into_any_element()
    }

    fn card(
        &self,
        header: gpui::Stateful<gpui::Div>,
        body: Option<AnyElement>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .overflow_hidden()
            .child(header)
            .children(body.map(|body| div().px_4().py_3().child(body)))
    }

    /// "login [bot] [Author] commented 2 days ago", with a link to GitHub.
    fn card_header(
        &self,
        id: &str,
        actor: Option<&Actor>,
        verb: &str,
        at: DateTime<Utc>,
        url: &str,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        let url = url.to_owned();
        div()
            .id(SharedString::from(format!("header-{id}")))
            .flex()
            .items_center()
            .gap_1p5()
            .min_h(px(36.))
            .pl_4()
            .pr_2()
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(actor_name(actor, theme))
            .when(actor.is_some_and(|a| a.is_bot), |el| {
                el.child(Badge::new("bot"))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(format!("{verb} {}", relative_time(at))),
            )
            .when(self.is_author(actor), |el| el.child(Badge::new("Author")))
            .child(
                IconButton::new(
                    SharedString::from(format!("open-{url}")),
                    IconName::ExternalLink,
                )
                .tooltip("Open on GitHub")
                .on_click(move |_, _, cx| {
                    // Not a click on the header, which may toggle.
                    cx.stop_propagation();
                    cx.open_url(&url);
                }),
            )
    }

    // --- Merge status -----------------------------------------------------

    fn merge_box(&self) -> AnyElement {
        let pr = self.pr();
        let theme = self.theme;
        let lead_color = match pr.merge_status {
            MergeStatus::Clean => Color::Success,
            MergeStatus::Conflicting => Color::Danger,
            _ => Color::Muted,
        };
        let rows: Vec<AnyElement> = [
            Some(self.reviews_row()),
            self.checks_row(),
            Some(self.mergeability_row()),
        ]
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(ix, row)| {
            div()
                .when(ix > 0, |el| {
                    el.border_t_1().border_color(theme.border_subtle)
                })
                .child(row)
                .into_any_element()
        })
        .collect();
        rail(
            event_icon(IconName::GitMerge, lead_color, theme),
            div()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .children(rows),
            theme,
        )
        .into_any_element()
    }

    fn reviews_row(&self) -> AnyElement {
        let pr = self.pr();
        let names = |status: ReviewerStatus| -> Vec<&str> {
            pr.reviewers
                .iter()
                .filter(|r| r.status == status)
                .map(|r| r.name.as_str())
                .collect()
        };
        let (approved, changes, requested) = (
            names(ReviewerStatus::Approved),
            names(ReviewerStatus::ChangesRequested),
            names(ReviewerStatus::Requested),
        );
        let waiting =
            (!requested.is_empty()).then(|| format!("Waiting on {}.", requested.join(", ")));
        let (icon, color, title, detail) = match pr.review_decision {
            Some(ReviewDecision::ChangesRequested) => (
                IconName::FileDiff,
                Color::Danger,
                "Changes requested",
                Some(format!("{} requested changes.", changes.join(", "))),
            ),
            Some(ReviewDecision::Approved) => (
                IconName::CircleCheck,
                Color::Success,
                "Changes approved",
                Some(format!("Approved by {}.", approved.join(", "))),
            ),
            Some(ReviewDecision::ReviewRequired) => (
                IconName::CircleDot,
                Color::Warning,
                "Review required",
                waiting.or(Some("An approving review is required to merge.".into())),
            ),
            // No branch protection: describe what reviews there are.
            None if !changes.is_empty() => (
                IconName::FileDiff,
                Color::Danger,
                "Changes requested",
                Some(format!("{} requested changes.", changes.join(", "))),
            ),
            None if !approved.is_empty() => (
                IconName::CircleCheck,
                Color::Success,
                "Approved",
                Some(format!("Approved by {}.", approved.join(", "))),
            ),
            None => (
                IconName::Eye,
                Color::Muted,
                "No approving reviews yet",
                waiting,
            ),
        };
        status_row(icon, color, title, detail, self.theme).into_any_element()
    }

    fn checks_row(&self) -> Option<AnyElement> {
        let pr = self.pr();
        let theme = self.theme;
        let hover = theme.element_hover;
        let summary = pr.check_summary;
        if summary.total() == 0 {
            return None;
        }
        let (failing, pending) = (summary.failing, summary.pending);
        let (icon, color, title) = if failing > 0 {
            (IconName::CircleX, Color::Danger, "Some checks failed")
        } else if pending > 0 {
            (IconName::CircleDashed, Color::Warning, "Checks in progress")
        } else {
            (IconName::CircleCheck, Color::Success, "All checks passed")
        };
        let detail = [
            (failing, "failing"),
            (pending, "in progress"),
            (summary.passing, "successful"),
            (summary.skipped, "skipped"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, label)| format!("{n} {label}"))
        .collect::<Vec<_>>()
        .join(", ");

        let key = SharedString::from("checks");
        let expanded = self.detail.is_expanded(&key, failing > 0);
        Some(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id(key.clone())
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(self.toggle(key))
                        .child(
                            status_row(icon, color, title, Some(detail), theme).child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .color(Color::Subtle),
                            ),
                        ),
                )
                .when(expanded, |el| {
                    el.child(
                        div()
                            .flex()
                            .flex_col()
                            .pb_1()
                            .children(
                                pr.checks
                                    .iter()
                                    .enumerate()
                                    .map(|(ix, check)| check_row(ix, check, theme)),
                            )
                            .when(summary.total() > pr.checks.len() as u64, |el| {
                                el.child(
                                    div()
                                        .pl(px(42.))
                                        .py_1()
                                        .text_xs()
                                        .text_color(theme.text_subtle)
                                        .child(format!(
                                            "{} more on GitHub",
                                            summary.total() - pr.checks.len() as u64
                                        )),
                                )
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    fn mergeability_row(&self) -> AnyElement {
        let pr = self.pr();
        let base = &pr.base_ref;
        let (icon, color, title, detail) = match pr.merge_status {
            MergeStatus::Clean => (
                IconName::GitMerge,
                Color::Success,
                "Ready to merge".to_owned(),
                Some(format!("No conflicts with {base}.")),
            ),
            MergeStatus::Unstable => (
                IconName::GitMerge,
                Color::Warning,
                "Can be merged".to_owned(),
                Some("Some checks aren't passing.".to_owned()),
            ),
            MergeStatus::Behind => (
                IconName::TriangleAlert,
                Color::Warning,
                format!("Out of date with {base}"),
                Some("Update the branch before merging.".to_owned()),
            ),
            MergeStatus::Blocked => (
                IconName::TriangleAlert,
                Color::Warning,
                "Merging is blocked".to_owned(),
                Some(format!("{base}'s protection rules aren't satisfied yet.")),
            ),
            MergeStatus::Conflicting => (
                IconName::TriangleAlert,
                Color::Danger,
                "This branch has conflicts".to_owned(),
                Some(format!("Resolve conflicts with {base} before merging.")),
            ),
            MergeStatus::Draft => (
                IconName::GitPullRequestDraft,
                Color::Muted,
                "This pull request is still a draft".to_owned(),
                None,
            ),
            MergeStatus::Unknown => (
                IconName::CircleDashed,
                Color::Muted,
                "Checking whether this can be merged…".to_owned(),
                None,
            ),
        };
        status_row(icon, color, title, detail, self.theme).into_any_element()
    }

    // --- Side panel -------------------------------------------------------

    fn side_panel(&self) -> impl IntoElement {
        let pr = self.pr();
        let theme = self.theme;
        let reviewers: Vec<AnyElement> = pr
            .reviewers
            .iter()
            .enumerate()
            .map(|(ix, reviewer)| reviewer_row(ix, reviewer, theme).into_any_element())
            .collect();
        let assignees: Vec<AnyElement> = pr
            .assignees
            .iter()
            .map(|assignee| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(24.))
                    .child(avatar(Some(assignee), 20., theme))
                    .child(div().truncate().child(assignee.login.clone()))
                    .into_any_element()
            })
            .collect();
        let labels = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children(pr.labels.iter().map(|label| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(20.))
                    .px_2()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(div().size(px(8.)).rounded_full().bg(rgb(label.color)))
                    .child(label.name.clone())
            }));

        div()
            .w(px(220.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_4()
            .pt_2()
            .child(side_section("Reviewers", reviewers, "No reviewers", theme))
            .child(side_section(
                "Assignees",
                assignees,
                "No one assigned",
                theme,
            ))
            .child(side_section(
                "Labels",
                if pr.labels.is_empty() {
                    Vec::new()
                } else {
                    vec![labels.into_any_element()]
                },
                "None yet",
                theme,
            ))
    }
}

/// A timeline row: `lead` (an avatar or icon) on the connecting line, and
/// the content beside it.
fn rail(lead: impl IntoElement, content: impl IntoElement, theme: &Theme) -> gpui::Div {
    div()
        .relative()
        .flex()
        .items_start()
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(RAIL_WIDTH / 2. - 1.))
                .w(px(2.))
                .bg(theme.border_subtle),
        )
        .child(
            div()
                .flex_none()
                .w(px(RAIL_WIDTH))
                .flex()
                .justify_center()
                .py_2()
                .child(lead),
        )
        .child(div().flex_1().min_w_0().py_2().child(content))
}

fn event_icon(icon: IconName, color: Color, theme: &Theme) -> impl IntoElement {
    // Opaque, so the timeline's line doesn't show through.
    div().rounded_full().bg(theme.background).child(
        div()
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(theme.border_subtle)
            .bg(color.subtle_fill(theme))
            .child(Icon::new(icon).size(IconSize::XSmall).color(color)),
    )
}

fn event_row(content: impl IntoElement, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .min_h(px(24.))
        .text_color(theme.text_muted)
        .child(div().flex_1().min_w_0().child(content))
}

/// A clickable row that expands or collapses what follows it.
fn disclosure(
    key: SharedString,
    expanded: bool,
    content: impl IntoElement,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let hover = theme.element_hover;
    div()
        .id(key)
        .flex()
        .items_center()
        .gap_1p5()
        .min_w_0()
        .min_h(px(24.))
        .px_1()
        .rounded_md()
        .text_color(theme.text_muted)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(IconSize::XSmall)
            .color(Color::Subtle),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// "**actor** action · 2 days ago", wrapping as one run of text.
fn sentence(
    actor: Option<&str>,
    action: &str,
    at: Option<DateTime<Utc>>,
    theme: &Theme,
) -> StyledText {
    let mut text = String::new();
    let mut highlights = Vec::new();
    if let Some(actor) = actor {
        text.push_str(actor);
        highlights.push((
            0..actor.len(),
            HighlightStyle {
                color: Some(theme.text),
                font_weight: Some(FontWeight::MEDIUM),
                ..Default::default()
            },
        ));
        text.push(' ');
    }
    text.push_str(action);
    if let Some(at) = at {
        let start = text.len();
        text.push_str(" · ");
        text.push_str(&relative_time(at));
        highlights.push((
            start..text.len(),
            HighlightStyle {
                color: Some(theme.text_subtle),
                ..Default::default()
            },
        ));
    }
    StyledText::new(text).with_highlights(highlights)
}

fn event_sentence(event: &Event, theme: &Theme) -> StyledText {
    sentence(
        Some(event.actor.as_deref().unwrap_or("ghost")),
        &event.kind.description(),
        Some(event.created_at),
        theme,
    )
}

fn commit_row(commit: &Commit, theme: &Theme) -> impl IntoElement {
    let url = commit.url.clone();
    let hover = theme.element_hover;
    div()
        .id(SharedString::from(commit.id.clone()))
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .h(px(24.))
        .px_1()
        .rounded_md()
        .when_some(url, |el, url| {
            el.cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |_, _, cx| cx.open_url(&url))
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text)
                .child(commit.headline.clone()),
        )
        .child(
            div()
                .flex_none()
                .font_family(MONOSPACE)
                .text_xs()
                .text_color(theme.text_subtle)
                .child(commit.short_oid.clone()),
        )
}

fn diff_hunk(thread_id: &str, hunk: &str, theme: &Theme) -> impl IntoElement {
    let lines: Vec<&str> = hunk.lines().filter(|l| !l.starts_with("@@")).collect();
    let tail = &lines[lines.len().saturating_sub(HUNK_CONTEXT_LINES)..];
    div()
        .id(SharedString::from(format!("hunk-{thread_id}")))
        .overflow_x_scroll()
        .py_1()
        .border_t_1()
        .border_color(theme.border_subtle)
        .font_family(MONOSPACE)
        .text_xs()
        .child(div().flex().flex_col().children(tail.iter().map(|line| {
            let fill = match line.chars().next() {
                Some('+') => Some(theme.success_subtle),
                Some('-') => Some(theme.danger_subtle),
                _ => None,
            };
            div()
                .px_3()
                .whitespace_nowrap()
                .when_some(fill, |el, fill| el.bg(fill))
                // Keep blank lines from collapsing.
                .child(if line.is_empty() { " " } else { line }.to_owned())
        })))
}

fn status_row(
    icon: IconName,
    color: Color,
    title: impl Into<SharedString>,
    detail: Option<String>,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2p5()
        .child(Icon::new(icon).size(IconSize::Medium).color(color))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().font_weight(FontWeight::MEDIUM).child(title.into()))
                .children(
                    detail.map(|detail| div().text_xs().text_color(theme.text_muted).child(detail)),
                ),
        )
}

fn check_row(ix: usize, check: &Check, theme: &Theme) -> impl IntoElement {
    let (icon, color, label) = match check.status {
        CheckStatus::Failure => (IconName::CircleX, Color::Danger, "Failing"),
        CheckStatus::Pending => (IconName::CircleDashed, Color::Warning, "In progress"),
        CheckStatus::Success => (IconName::CircleCheck, Color::Success, "Successful"),
        CheckStatus::Neutral => (IconName::CircleDot, Color::Subtle, "Skipped"),
    };
    let url = check.url.clone();
    let hover = theme.element_hover;
    div()
        .id(("check", ix))
        .flex()
        .items_center()
        .gap_2()
        .h(px(28.))
        .pl(px(42.))
        .pr_3()
        .text_xs()
        .when_some(url, |el, url| {
            el.cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |_, _, cx| cx.open_url(&url))
        })
        .child(Icon::new(icon).size(IconSize::XSmall).color(color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(check.name.clone()),
        )
        .child(div().flex_none().text_color(theme.text_subtle).child(label))
}

fn reviewer_row(ix: usize, reviewer: &Reviewer, theme: &Theme) -> impl IntoElement {
    let (icon, color, label) = match reviewer.status {
        ReviewerStatus::Requested => (IconName::CircleDot, Color::Warning, "Awaiting review"),
        ReviewerStatus::Approved => (IconName::Check, Color::Success, "Approved"),
        ReviewerStatus::ChangesRequested => {
            (IconName::FileDiff, Color::Danger, "Requested changes")
        }
        ReviewerStatus::Commented => (IconName::MessageSquare, Color::Muted, "Left comments"),
    };
    div()
        .flex()
        .items_center()
        .gap_2()
        .h(px(24.))
        .child(match &reviewer.avatar_url {
            Some(url) => img(url.clone())
                .flex_none()
                .size(px(20.))
                .rounded_full()
                .bg(theme.element_selected)
                .into_any_element(),
            None => avatar(None, 20., theme).into_any_element(),
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(reviewer.name.clone()),
        )
        .child(
            div()
                .id(("reviewer-status", ix))
                .tooltip(move |_, cx| Tooltip::view(label.into(), cx))
                .child(Icon::new(icon).color(color)),
        )
}

fn side_section(
    title: &'static str,
    children: Vec<AnyElement>,
    empty: &'static str,
    theme: &Theme,
) -> impl IntoElement {
    let is_empty = children.is_empty();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .pb_4()
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text_subtle)
                .child(title),
        )
        .when(is_empty, |el| {
            el.child(div().text_color(theme.text_muted).child(empty))
        })
        .children(children)
}

fn actor_name(actor: Option<&Actor>, theme: &Theme) -> impl IntoElement {
    div()
        .flex_none()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(actor.map_or("ghost".to_owned(), |a| a.login.clone()))
}

fn avatar(actor: Option<&Actor>, size: f32, theme: &Theme) -> AnyElement {
    match actor.and_then(|a| a.avatar_url.clone()) {
        Some(url) => img(url)
            .flex_none()
            .size(px(size))
            .rounded_full()
            .bg(theme.element_selected)
            .into_any_element(),
        None => div()
            .flex_none()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .child(Icon::new(IconName::User).size(IconSize::XSmall))
            .into_any_element(),
    }
}

pub(super) fn branch(name: &str, theme: &Theme) -> impl IntoElement {
    div()
        .px_1p5()
        .rounded_md()
        .bg(theme.accent_subtle)
        .text_color(theme.text_accent)
        .font_family(MONOSPACE)
        .text_xs()
        .child(name.to_owned())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, minute, 0).unwrap()
    }

    fn event(id: &str, kind: EventKind) -> TimelineItem {
        TimelineItem::Event(Event {
            id: id.into(),
            actor: Some("octocat".into()),
            created_at: at(0),
            kind,
        })
    }

    fn commit(id: &str) -> TimelineItem {
        TimelineItem::Commit(Commit {
            id: id.into(),
            url: None,
            short_oid: "abc1234".into(),
            headline: "Fix it".into(),
            author: Some("octocat".into()),
            committed_at: at(1),
        })
    }

    fn review(id: &str, state: ReviewState, body: &str) -> TimelineItem {
        TimelineItem::Review(Review {
            id: id.into(),
            url: String::new(),
            author: None,
            state,
            body: body.into(),
            submitted_at: at(2),
            threads: Vec::new(),
        })
    }

    fn shape(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Comment(c) => format!("comment {}", c.id),
                Entry::Review(r) => format!("review {}", r.id),
                Entry::Commits(c) => format!("{} commits", c.len()),
                Entry::Event(e) => format!("event {}", e.id),
                Entry::Events(e) => format!("{} events", e.len()),
            })
            .collect()
    }

    #[test]
    fn collapses_runs_of_bookkeeping() {
        let timeline = [
            event("r1", EventKind::ReviewRequested { reviewer: None }),
            event("d1", EventKind::Deployed { environment: None }),
            event(
                "l1",
                EventKind::Labeled {
                    label: "bug".into(),
                },
            ),
            commit("c1"),
            commit("c2"),
            event("ready", EventKind::ReadyForReview),
            event("d2", EventKind::Deployed { environment: None }),
            review("approve", ReviewState::Approved, ""),
            // Replies to another review's threads: shown in those threads.
            review("reply", ReviewState::Commented, ""),
            event(
                "merged",
                EventKind::Merged {
                    commit: None,
                    base: "main".into(),
                },
            ),
            event("closed", EventKind::Closed),
        ];
        assert_eq!(
            shape(&group_timeline(&timeline)),
            [
                "3 events",
                "2 commits",
                "event ready",
                "1 events",
                "review approve",
                "event merged",
            ]
        );
    }

    #[test]
    fn summarizes_event_categories_in_order() {
        let timeline = [
            event("d1", EventKind::Deployed { environment: None }),
            event("r1", EventKind::ReviewRequested { reviewer: None }),
            event("d2", EventKind::Deployed { environment: None }),
        ];
        let entries = group_timeline(&timeline);
        let [Entry::Events(events)] = entries.as_slice() else {
            panic!("expected one group");
        };
        assert_eq!(summarize_events(events), "deployments, review requests");
    }

    mod links {
        use std::sync::Arc;

        use gpui::{Modifiers, TestAppContext, point, px};

        use crate::OpenLink;
        use crate::github::FakeGithub;
        use crate::state::{DiffLoad, PrTab, SectionKind};
        use crate::test_support::{VisualTestContextExt as _, detail, open_docket, pull_request};

        const LINKED: &str = "https://github.com/acme/widgets/pull/8";

        #[gpui::test]
        fn a_link_to_a_pull_request_opens_it_here(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            body.body = format!("[Follows #8]({LINKED})");
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            // Not in the docket.
            let linked = pull_request("acme/widgets", 8, "Add the gears");
            github.set_detail("acme/widgets".parse().unwrap(), detail(&linked));
            let (tab, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);

            // The paragraph is all link, so its first character is too.
            let paragraph = cx
                .bounds_of(&format!("paragraph-linking-{LINKED}"))
                .expect("the description is drawn");
            cx.simulate_click(paragraph.origin + point(px(2.), px(4.)), Modifiers::none());
            cx.run_until_parked();
            tab.read_with(cx, |tab, _| {
                let detail = tab.detail.as_ref().expect("a pull request is open");
                assert_eq!(detail.number, 8);
                assert!(detail.loaded.is_some(), "{:?}", detail.error);
                let summary = detail.summary.as_ref().expect("it loaded");
                assert_eq!(summary.head_ref_oid, linked.head_ref_oid);
            });
            assert_eq!(cx.opened_url(), None);

            // Back to one in the docket, at its files.
            cx.dispatch_action(OpenLink {
                url: "https://github.com/acme/widgets/pull/7/files".into(),
            });
            cx.run_until_parked();
            tab.read_with(cx, |tab, _| {
                let detail = tab.detail.as_ref().expect("a pull request is open");
                assert_eq!(detail.number, 7);
                assert_eq!(detail.tab, PrTab::Files);
                assert_eq!(detail.summary.as_ref().map(|pr| &pr.id), Some(&pr.id));
            });
        }

        #[gpui::test]
        fn its_files_wait_for_its_commits(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let linked = pull_request("acme/widgets", 8, "Add the gears");
            github.set_detail("acme/widgets".parse().unwrap(), detail(&linked));
            let (tab, cx) = open_docket(cx, github, Vec::new());

            tab.update(cx, |tab, cx| tab.open_link(&format!("{LINKED}/files"), cx));
            tab.read_with(cx, |tab, _| {
                let detail = tab.detail.as_ref().expect("it opens at once");
                assert!(detail.summary.is_none());
                let files = detail.files.as_ref().expect("the Files tab is open");
                assert!(matches!(files.diff, DiffLoad::Loading));
            });
            assert_eq!(cx.update(|_, cx| tab.read(cx).title(cx)), "acme/widgets#8");

            cx.run_until_parked();
            tab.read_with(cx, |tab, _| {
                let files = tab.detail.as_ref().and_then(|d| d.files.as_ref()).unwrap();
                // Loaded, as far as it can without a project.
                assert!(matches!(files.diff, DiffLoad::NoProject));
            });
        }

        #[gpui::test]
        fn other_links_open_in_the_browser(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let (tab, cx) = open_docket(cx, github, Vec::new());
            let issue = "https://github.com/acme/widgets/issues/8";
            cx.dispatch_action(OpenLink { url: issue.into() });
            cx.run_until_parked();
            assert_eq!(cx.opened_url().as_deref(), Some(issue));
            assert!(tab.read_with(cx, |tab, _| tab.detail.is_none()));
        }
    }
}
