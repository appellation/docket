//! The open pull request: its conversation, loosely after GitHub's (the
//! description, the timeline, then where it stands on reviews and merging,
//! with reviewers, checks, and assignees alongside), its changes, and all
//! its files at its head (the Browse tab).
//!
//! Bookkeeping (review requests, deployments, labels, force-pushes…), bot
//! and hidden comments, and reviews whose threads are all resolved or
//! outdated are noise: each run of it collapses to one row. Comments by a
//! project's automated accounts (`Project::automated_authors`) count as bot
//! comments. Bot reviews are not noise, until settled: they're feedback on
//! the code. Everything between comments and reviews with something to read
//! (those runs, commits, state changes, bare approvals) then folds into one
//! row, which opens to them indented, so the discussion stands out.

use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, ClipboardItem, Entity, FontWeight, HighlightStyle, SharedString, StyledText,
    Window, div, img, prelude::*, px,
};

use super::commit_list::CommitList;
use super::components::{
    Badge, Button, ButtonStyle, Callout, CalloutKind, Icon, IconButton, IconName, IconSize,
    MONOSPACE, Markdown, NestedScroll, Spinner, TabBar, TabBarItem, TestSelector as _, Tooltip,
    relative_time,
};
use super::diff_view::{DiffView, no_project};
use super::file_browser::FileBrowserView;
use super::pr_actions::pr_actions;
use super::theme::{Color, Theme};
use crate::github::{
    Actor, Check, CheckStatus, Comment, Commit, Event, EventKind, MergeStatus, PullRequestDetail,
    PullRequestState, Review, ReviewDecision, ReviewState, ReviewThread, Reviewer, ReviewerStatus,
    TimelineItem,
};
use crate::state::{DiffLoad, LoadedDetail, PrDetail, PrTab, Tab};

/// Width of the column holding avatars and event icons, which the
/// timeline's connecting line runs down the middle of.
const RAIL_WIDTH: f32 = 40.;
/// How far a folded row's rows are indented: their icons sit under its
/// chevron.
const NEST_INDENT: f32 = 30.;
/// Lines of a review thread's diff hunk to show above its comments.
const HUNK_CONTEXT_LINES: usize = 4;
/// The most checks a group in the side panel shows before it scrolls.
const CHECK_GROUP_ROWS: usize = 5;
const CHECK_ROW_HEIGHT: f32 = 24.;
/// Where a check's name starts: past its group's icon, under its title.
const CHECK_INDENT: f32 = 28.;

#[derive(IntoElement)]
pub struct PrDetailView {
    state: Entity<Tab>,
    /// The Files tab. It keeps scroll position and collapsed files, so it's
    /// an entity owned by the workspace rather than rebuilt each frame.
    diff_view: Option<Entity<DiffView>>,
    /// The Browse tab, likewise, once its head is fetched.
    browser: Option<Entity<FileBrowserView>>,
}

impl PrDetailView {
    pub fn new(
        state: Entity<Tab>,
        diff_view: Option<Entity<DiffView>>,
        browser: Option<Entity<FileBrowserView>>,
    ) -> Self {
        Self {
            state,
            diff_view,
            browser,
        }
    }
}

impl PrDetailView {
    /// The open pull request's check groups, kept across frames (and its
    /// tabs) so they stay where they were scrolled to.
    fn check_lists(&self, window: &mut Window, cx: &mut App) -> NestedScroll {
        let Some(detail) = &self.state.read(cx).detail else {
            return NestedScroll::default();
        };
        let key = format!("check-lists-{}#{}", detail.repo, detail.number);
        window
            .use_keyed_state(SharedString::from(key), cx, |_, _| NestedScroll::default())
            .read(cx)
            .clone()
    }
}

impl RenderOnce for PrDetailView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let check_lists = self.check_lists(window, cx);
        let theme = Theme::get(cx);
        let root = div().flex_1().min_w_0().h_full().flex().flex_col();
        let Some(detail) = &self.state.read(cx).detail else {
            return root;
        };
        let header = header(detail, &self.state, theme, cx);
        let tabs = tabs(detail, &self.state);
        if detail.tab == PrTab::Files {
            return root.child(header).child(tabs).children(self.diff_view);
        }
        if detail.tab == PrTab::Browse {
            let body = match self.browser {
                Some(browser) => browser.into_any_element(),
                None => browse_pending(detail, &self.state, theme),
            };
            return root.child(header).child(tabs).child(body);
        }

        let body = match &detail.loaded {
            Some(loaded) if detail.tab == PrTab::Commits => commits_tab(loaded).into_any_element(),
            Some(loaded) => {
                let automated_authors = self
                    .state
                    .read(cx)
                    .app
                    .read(cx)
                    .automated_authors(&detail.repo);
                let actions = pr_actions(&loaded.pr, &self.state.read(cx).app, cx);
                let view = View {
                    theme,
                    state: &self.state,
                    detail,
                    loaded,
                    automated_authors: &automated_authors,
                    check_lists,
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
                            .child(view.side_panel(actions)),
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
        .child(tab(
            PrTab::Browse,
            "pr-tab-browse",
            IconName::FolderTree,
            "Browse",
        ))
}

/// The Browse tab until there are files to browse: they're read from the
/// head its diff fetches.
fn browse_pending(detail: &PrDetail, state: &Entity<Tab>, theme: &Theme) -> AnyElement {
    match detail.files.as_ref().map(|files| &files.diff) {
        Some(DiffLoad::Failed(err)) => div()
            .p_4()
            .child(Callout::new(CalloutKind::Error, err.clone()).title("Couldn't load its files"))
            .into_any_element(),
        Some(DiffLoad::NoProject) => no_project(detail, state).into_any_element(),
        _ => div()
            .flex()
            .justify_center()
            .gap_2()
            .pt(px(96.))
            .text_color(theme.text_muted)
            .child(Spinner::new("pr-browse-loading"))
            .child("Fetching its files into your checkout…")
            .into_any_element(),
    }
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
                .on_click(move |_, _, cx| crate::ui::open_in_browser(&url, cx)),
        )
    });
    CommitList::new(SharedString::from(format!("pr-commits-{}", pr.id)), commits).notice(notice)
}

fn header(detail: &PrDetail, state: &Entity<Tab>, theme: &Theme, cx: &App) -> impl IntoElement {
    let url = detail.url();
    let (back, refresh) = (state.clone(), state.clone());
    // The Browse tab re-listing its files.
    let browsing = state
        .read(cx)
        .browser
        .as_ref()
        .is_some_and(|browser| browser.reloading);
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
                        // The diff loads behind the other tabs too.
                        || (detail.tab == PrTab::Files
                            && detail.files.as_ref().is_some_and(|files| files.is_busy()))
                        || (detail.tab == PrTab::Browse && browsing),
                    |el| el.child(Spinner::new("pr-detail-refreshing")),
                )
                .child(
                    Button::new("pr-open-github", "Open on GitHub")
                        .icon(IconName::ExternalLink)
                        .style(ButtonStyle::Secondary)
                        .on_click(move |_, _, cx| crate::ui::open_in_browser(&url, cx)),
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
            .children(pr.author.as_ref().map(|a| {
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(a.display_name().to_owned())
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
    /// Consecutive noise (bookkeeping events, bot and hidden comments,
    /// resolved reviews), collapsed together.
    Quiet(Vec<Quiet<'a>>),
    /// Everything between discussion (comments and reviews with something
    /// to read), folded into one row: never itself a `Comment`, a `Review`
    /// with a body or threads, or another `Folded`.
    Folded(Vec<Entry<'a>>),
}

impl Entry<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Comment(comment) => &comment.id,
            Self::Review(review) => &review.id,
            Self::Commits(commits) => &commits[0].id,
            Self::Event(event) => &event.id,
            Self::Quiet(run) => run[0].id(),
            Self::Folded(entries) => entries[0].id(),
        }
    }

    /// Discussion, which stays out of folded rows.
    fn is_discussion(&self) -> bool {
        match self {
            Self::Comment(_) => true,
            Self::Review(review) => !is_bare(review),
            _ => false,
        }
    }
}

/// Something the timeline folds away with whatever noise is next to it.
#[derive(Clone, Copy, Debug)]
enum Quiet<'a> {
    Event(&'a Event),
    Comment(&'a Comment),
    Review(&'a Review),
}

impl Quiet<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Event(event) => &event.id,
            Self::Comment(comment) => &comment.id,
            Self::Review(review) => &review.id,
        }
    }

    /// A short plural noun for summarizing a run.
    fn category(&self) -> &'static str {
        match self {
            Self::Event(event) => event.kind.category(),
            Self::Comment(comment) if comment.minimized.is_some() => "hidden comments",
            Self::Comment(_) => "bot comments",
            Self::Review(_) => "resolved reviews",
        }
    }
}

/// Comments collapsed by default: noise, rather than discussion. Those by
/// `automated_authors` are taken as a bot's.
fn is_quiet(comment: &Comment, automated_authors: &[String]) -> bool {
    comment.minimized.is_some() || is_automated(comment, automated_authors)
}

/// Comments by a bot, or by one of `automated_authors`.
fn is_automated(comment: &Comment, automated_authors: &[String]) -> bool {
    comment.is_by_bot()
        || comment.author.as_ref().is_some_and(|author| {
            automated_authors
                .iter()
                .any(|login| login.eq_ignore_ascii_case(&author.login))
        })
}

/// Threads that no longer need attention: resolved, or left on code that has
/// since changed. GitHub folds them.
fn is_settled(thread: &ReviewThread) -> bool {
    thread.is_resolved || thread.is_outdated
}

/// Reviews collapsed by default: comments whose every thread is resolved or
/// outdated, so nothing in them needs attention. An approval or request for
/// changes still says where the pull request stands, so it stays.
fn is_resolved(review: &Review) -> bool {
    matches!(
        review.state,
        ReviewState::Commented | ReviewState::Dismissed
    ) && !review.threads.is_empty()
        && review.threads.iter().all(is_settled)
}

/// A review with nothing to read: just an approval or request for changes,
/// shown as a one-line event.
fn is_bare(review: &Review) -> bool {
    review.body.trim().is_empty() && review.threads.is_empty()
}

/// The timeline as shown: runs of noise collapsed, then everything between
/// discussion folded. Comments by `automated_authors` are noise.
fn timeline_entries<'a>(items: &'a [TimelineItem], automated_authors: &[String]) -> Vec<Entry<'a>> {
    fold_timeline(group_timeline(items, automated_authors))
}

fn group_timeline<'a>(items: &'a [TimelineItem], automated_authors: &[String]) -> Vec<Entry<'a>> {
    fn push_quiet<'a>(entries: &mut Vec<Entry<'a>>, item: Quiet<'a>) {
        match entries.last_mut() {
            Some(Entry::Quiet(run)) => run.push(item),
            _ => entries.push(Entry::Quiet(vec![item])),
        }
    }

    let mut entries = Vec::new();
    for item in items {
        match item {
            TimelineItem::Comment(comment) if is_quiet(comment, automated_authors) => {
                push_quiet(&mut entries, Quiet::Comment(comment))
            }
            TimelineItem::Comment(comment) => entries.push(Entry::Comment(comment)),
            // A bare comment review is just replies to other reviews'
            // threads, which show in those threads.
            TimelineItem::Review(review)
                if review.state == ReviewState::Commented
                    && review.threads.is_empty()
                    && review.body.trim().is_empty() => {}
            TimelineItem::Review(review) if is_resolved(review) => {
                push_quiet(&mut entries, Quiet::Review(review))
            }
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
            TimelineItem::Event(event) => push_quiet(&mut entries, Quiet::Event(event)),
        }
    }
    entries
}

/// Folds each run of entries between discussion into one, unless the run is
/// just one entry.
fn fold_timeline(entries: Vec<Entry<'_>>) -> Vec<Entry<'_>> {
    fn flush<'a>(folded: &mut Vec<Entry<'a>>, run: &mut Vec<Entry<'a>>) {
        match run.len() {
            0 => {}
            1 => folded.append(run),
            _ => folded.push(Entry::Folded(std::mem::take(run))),
        }
    }

    let mut folded = Vec::new();
    let mut run = Vec::new();
    for entry in entries {
        if entry.is_discussion() {
            flush(&mut folded, &mut run);
            folded.push(entry);
        } else {
            run.push(entry);
        }
    }
    flush(&mut folded, &mut run);
    folded
}

/// "2 commits, 9 events · deployments, approvals": how many commits and
/// other events a folded row holds, and what the events are, in the order
/// each first appears.
fn summarize_folded(entries: &[Entry]) -> (String, String) {
    fn review_category(review: &Review) -> &'static str {
        match review.state {
            ReviewState::Approved => "approvals",
            ReviewState::ChangesRequested => "change requests",
            ReviewState::Dismissed => "dismissed reviews",
            ReviewState::Commented | ReviewState::Pending => "reviews",
        }
    }

    let mut commits = 0;
    let mut events = 0;
    let mut categories: Vec<&str> = Vec::new();
    let mut add = |category: &'static str| {
        if !categories.contains(&category) {
            categories.push(category);
        }
    };
    for entry in entries {
        match entry {
            Entry::Commits(run) => commits += run.len(),
            Entry::Event(event) => {
                events += 1;
                add(event.kind.category());
            }
            Entry::Review(review) => {
                events += 1;
                add(review_category(review));
            }
            Entry::Quiet(run) => {
                events += run.len();
                run.iter().for_each(|item| add(item.category()));
            }
            Entry::Comment(_) | Entry::Folded(_) => {}
        }
    }
    let plural = |n: usize, noun: &str| match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    };
    let count = [(commits, "commit"), (events, "event")]
        .into_iter()
        .filter(|&(n, _)| n > 0)
        .map(|(n, noun)| plural(n, noun))
        .collect::<Vec<_>>()
        .join(", ");
    (count, categories.join(", "))
}

/// "review requests, bot comments": what a run of noise is made of, in the
/// order each first appears.
fn summarize_quiet(run: &[Quiet]) -> String {
    let mut categories: Vec<&str> = Vec::new();
    for item in run {
        let category = item.category();
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
    /// Logins whose comments the project counts as a bot's.
    automated_authors: &'a [String],
    /// The check groups, which scroll inside the page.
    check_lists: NestedScroll,
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
                    .child(author.display_name().to_owned())
            }))
            .child(action)
            .child(branch("branch-base", &pr.base_ref, theme))
            .child("from")
            .child(branch("branch-head", &pr.head_ref, theme))
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
            // `flex_1`'s basis is 0%, which counts as the content's width
            // while the row's width is unknown (once the conversation is
            // wide enough to be capped), and the description then lays out
            // unwrapped, under the side panel.
            .flex_basis(px(0.))
            .min_w_0()
            .child(self.description())
            .children(
                timeline_entries(&pr.timeline, self.automated_authors)
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
            Entry::Quiet(run) => self.quiet(&run),
            Entry::Folded(entries) => self.folded(entries),
        }
    }

    /// Everything between discussion: one row summarizing it, which
    /// expands to its entries, indented below it.
    fn folded(&self, entries: Vec<Entry>) -> AnyElement {
        let theme = self.theme;
        let key = SharedString::from(format!("folded-{}", entries[0].id()));
        let expanded = self.detail.is_expanded(&key, false);
        let (count, categories) = summarize_folded(&entries);
        let summary = if categories.is_empty() {
            count.clone()
        } else {
            format!("{count} · {categories}")
        };
        let summary = StyledText::new(summary).with_highlights([(
            0..count.len(),
            HighlightStyle {
                color: Some(theme.text),
                font_weight: Some(FontWeight::MEDIUM),
                ..Default::default()
            },
        )]);
        div()
            .flex()
            .flex_col()
            .child(rail(
                event_icon(IconName::Ellipsis, Color::Subtle, theme),
                disclosure(key.clone(), expanded, summary, theme).on_click(self.toggle(key)),
                theme,
            ))
            .when(expanded, |el| {
                el.child(nested(
                    entries.into_iter().map(|entry| self.entry(entry)),
                    theme,
                ))
            })
            .into_any_element()
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
        let collapsible = is_quiet(comment, self.automated_authors);
        let key = SharedString::from(comment.id.clone());
        let expanded = self.detail.is_expanded(&key, !collapsible);

        if !expanded {
            let name = comment.author.as_ref().map_or("ghost", Actor::display_name);
            let summary = match &comment.minimized {
                Some(reason) if reason.is_empty() => {
                    format!("{name}'s comment was hidden")
                }
                Some(reason) => format!("{name}'s comment was hidden as {reason}"),
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
                                .child(name.to_owned()),
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
            let name = review.author.as_ref().map(Actor::display_name);
            return rail(
                event_icon(icon, color, theme),
                event_row(
                    sentence(name, verb, Some(review.submitted_at), theme),
                    theme,
                ),
                theme,
            )
            .into_any_element();
        }

        let collapsible = is_resolved(review);
        let key = SharedString::from(review.id.clone());
        if !self.detail.is_expanded(&key, !collapsible) {
            let name = review.author.as_ref().map_or("ghost", Actor::display_name);
            let kind = if review.threads.iter().all(|t| t.is_resolved) {
                "resolved "
            } else if review.threads.iter().all(|t| t.is_outdated) {
                "outdated "
            } else {
                ""
            };
            let threads = match review.threads.len() {
                1 => format!("1 {kind}thread"),
                n => format!("{n} {kind}threads"),
            };
            let row = div()
                .flex()
                .items_center()
                .gap_1p5()
                .min_w_0()
                .text_color(theme.text_muted)
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(name.to_owned()),
                )
                .when(review.author.as_ref().is_some_and(|a| a.is_bot), |el| {
                    el.child(Badge::new("bot"))
                })
                .child(div().flex_none().child(verb))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.text_subtle)
                        .child(relative_time(review.submitted_at)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.text_subtle)
                        .child(format!("· {threads}")),
                )
                .children(doc.preview().map(|preview| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.text_subtle)
                        .child(preview)
                }));
            return rail(
                avatar(review.author.as_ref(), 24., theme),
                disclosure(key.clone(), false, row, theme).on_click(self.toggle(key)),
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
            .child(Icon::new(icon).color(color))
            .when(collapsible, |el| {
                el.cursor_pointer()
                    .on_click(self.toggle(key.clone()))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(IconSize::XSmall)
                            .color(Color::Subtle),
                    )
            });
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
        let expanded = self.detail.is_expanded(&key, !is_settled(thread));

        let header =
            div()
                .test_selector(|| key.to_string())
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

    /// One bookkeeping event, on its own row.
    fn minor_event(&self, event: &Event) -> AnyElement {
        let theme = self.theme;
        rail(
            event_icon(IconName::Ellipsis, Color::Subtle, theme),
            event_row(event_sentence(event, theme), theme),
            theme,
        )
        .into_any_element()
    }

    fn quiet_item(&self, item: Quiet) -> AnyElement {
        match item {
            Quiet::Event(event) => self.minor_event(event),
            Quiet::Comment(comment) => self.comment(comment),
            Quiet::Review(review) => self.review(review),
        }
    }

    /// A run of noise: one row summarizing it, which expands to the run's
    /// own rows below it.
    fn quiet(&self, run: &[Quiet]) -> AnyElement {
        let theme = self.theme;
        if let [item] = run {
            return self.quiet_item(*item);
        }

        let key = SharedString::from(format!("events-{}", run[0].id()));
        let expanded = self.detail.is_expanded(&key, false);
        let count = format!("{} events", run.len());
        let summary = format!("{count} · {}", summarize_quiet(run));
        let summary = StyledText::new(summary).with_highlights([(
            0..count.len(),
            HighlightStyle {
                color: Some(theme.text),
                font_weight: Some(FontWeight::MEDIUM),
                ..Default::default()
            },
        )]);
        div()
            .flex()
            .flex_col()
            .child(rail(
                event_icon(IconName::Ellipsis, Color::Subtle, theme),
                disclosure(key.clone(), expanded, summary, theme).on_click(self.toggle(key)),
                theme,
            ))
            .when(expanded, |el| {
                el.child(nested(run.iter().map(|item| self.quiet_item(*item)), theme))
            })
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
        let id = SharedString::from(format!("header-{id}"));
        div()
            .test_selector(|| id.to_string())
            .id(id.clone())
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
                    crate::ui::open_in_browser(&url, cx);
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
        let rows: Vec<AnyElement> = [self.reviews_row(), self.mergeability_row()]
            .into_iter()
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

    /// Merging and closing (`actions`, while it's open), then reviewers,
    /// checks, and assignees.
    fn side_panel(&self, actions: Option<AnyElement>) -> impl IntoElement {
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
                    .child(div().truncate().child(assignee.display_name().to_owned()))
                    .into_any_element()
            })
            .collect();
        div()
            .test_selector(|| "side-panel".to_owned())
            .w(px(220.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_4()
            .pt_2()
            .children(actions)
            .child(side_section("Reviewers", reviewers, "No reviewers", theme))
            .child(side_section(
                "Checks",
                self.check_groups(),
                "No checks",
                theme,
            ))
            // Most pull requests have no one assigned, so say nothing then.
            .when(!assignees.is_empty(), |el| {
                el.child(side_section("Assignees", assignees, "", theme))
            })
    }

    /// The head commit's checks, grouped by status, most urgent first.
    fn check_groups(&self) -> Vec<AnyElement> {
        let summary = self.pr().check_summary;
        let mut groups: Vec<AnyElement> = [
            (CheckStatus::Failure, summary.failing),
            (CheckStatus::Pending, summary.pending),
            (CheckStatus::Success, summary.passing),
            (CheckStatus::Neutral, summary.skipped),
        ]
        .into_iter()
        .filter_map(|(status, count)| self.check_group(status, count))
        .collect();
        if !groups.is_empty() {
            groups.push(self.check_lists.tracker().into_any_element());
        }
        groups
    }

    /// One status's checks, under a header that collapses them. Failing and
    /// pending checks start expanded; the rest, which need nothing from
    /// you, start collapsed. Past `CHECK_GROUP_ROWS` they scroll.
    fn check_group(&self, status: CheckStatus, count: u64) -> Option<AnyElement> {
        let theme = self.theme;
        let hover = theme.element_hover;
        let checks: Vec<_> = self
            .pr()
            .checks
            .iter()
            .enumerate()
            .filter(|(_, check)| check.status == status)
            .collect();
        // `count` covers checks past the first page.
        let count = count.max(checks.len() as u64);
        if count == 0 {
            return None;
        }
        let (icon, color, title) = check_status(status);
        let key = SharedString::from(format!("checks-{}", check_status_id(status)));
        let expanded = self.detail.is_expanded(
            &key,
            matches!(status, CheckStatus::Failure | CheckStatus::Pending),
        );
        let more = count - checks.len() as u64;
        let header = div()
            .id(key.clone())
            .test_selector(|| key.to_string())
            .flex()
            .items_center()
            .gap_2()
            .h(px(CHECK_ROW_HEIGHT))
            .px_1()
            .rounded_md()
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(self.toggle(key.clone()))
            .child(Icon::new(icon).size(IconSize::Small).color(color))
            .child(div().flex_1().min_w_0().truncate().child(title))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.text_subtle)
                    .child(count.to_string()),
            )
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::Small)
                .color(Color::Subtle),
            );
        Some(
            div()
                .flex()
                .flex_col()
                .child(header)
                .when(expanded, |el| {
                    el.child(
                        self.check_lists.list(
                            status as usize,
                            div()
                                .id(SharedString::from(format!("{key}-list")))
                                .test_selector(|| format!("{key}-list"))
                                .flex()
                                .flex_col()
                                .max_h(px(CHECK_ROW_HEIGHT * CHECK_GROUP_ROWS as f32))
                                .overflow_y_scroll()
                                .children(
                                    checks
                                        .into_iter()
                                        .map(|(ix, check)| check_row(ix, check, theme)),
                                ),
                        ),
                    )
                    .when(more > 0, |el| {
                        el.child(
                            div()
                                .pl(px(CHECK_INDENT))
                                .h(px(CHECK_ROW_HEIGHT))
                                .flex()
                                .items_center()
                                .text_xs()
                                .text_color(theme.text_subtle)
                                .child(format!("{more} more on GitHub")),
                        )
                    })
                })
                .into_any_element(),
        )
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

/// Rows that opened from the row above them, indented, with the timeline's
/// line carrying on beside them.
fn nested(rows: impl IntoIterator<Item = AnyElement>, theme: &Theme) -> gpui::Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(RAIL_WIDTH / 2. - 1.))
                .w(px(2.))
                .bg(theme.border_subtle),
        )
        .child(div().flex().flex_col().pl(px(NEST_INDENT)).children(rows))
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
        .test_selector(|| key.to_string())
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
    let id = SharedString::from(commit.id.clone());
    div()
        .test_selector(|| id.to_string())
        .id(id.clone())
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
                .on_click(move |_, _, cx| crate::ui::open_in_browser(&url, cx))
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

fn check_status(status: CheckStatus) -> (IconName, Color, &'static str) {
    match status {
        CheckStatus::Failure => (IconName::CircleX, Color::Danger, "Failing"),
        CheckStatus::Pending => (IconName::CircleDashed, Color::Warning, "In progress"),
        CheckStatus::Success => (IconName::CircleCheck, Color::Success, "Successful"),
        CheckStatus::Neutral => (IconName::CircleDot, Color::Subtle, "Skipped"),
    }
}

fn check_status_id(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Failure => "failing",
        CheckStatus::Pending => "pending",
        CheckStatus::Success => "successful",
        CheckStatus::Neutral => "skipped",
    }
}

/// A check in its group, which opens its details on GitHub.
fn check_row(ix: usize, check: &Check, theme: &Theme) -> impl IntoElement {
    let url = check.url.clone();
    let name = SharedString::from(check.name.clone());
    let hover = theme.element_hover;
    div()
        .id(("check", ix))
        .test_selector(|| format!("check-{}", check.name))
        .flex_none()
        .flex()
        .items_center()
        .h(px(CHECK_ROW_HEIGHT))
        .pl(px(CHECK_INDENT))
        .pr_1()
        .rounded_md()
        .text_xs()
        .text_color(theme.text_muted)
        .when_some(url, |el, url| {
            el.cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |_, _, cx| crate::ui::open_in_browser(&url, cx))
        })
        .tooltip({
            let name = name.clone();
            move |_, cx| Tooltip::view(name.clone(), cx)
        })
        .child(div().min_w_0().truncate().child(name))
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
        .id(title)
        .test_selector(|| format!("side-{}", title.to_lowercase()))
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
        .child(actor.map_or("ghost", Actor::display_name).to_owned())
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

/// A branch's name as a pill, which copies the name when clicked. `id`
/// tells apart pills in one view whose names may match (a base and head).
pub(super) fn branch(id: &'static str, name: &str, theme: &Theme) -> impl IntoElement {
    let name = SharedString::from(name.to_owned());
    let hover = theme.accent_subtle.alpha(theme.accent_subtle.a * 2.);
    div()
        .id(id)
        .test_selector(move || id.to_owned())
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .rounded_md()
        .bg(theme.accent_subtle)
        .hover(move |el| el.bg(hover))
        .cursor_pointer()
        .text_color(theme.text_accent)
        .font_family(MONOSPACE)
        .text_xs()
        .child(name.clone())
        .child(
            Icon::new(IconName::Copy)
                .size(IconSize::XSmall)
                .color(Color::Accent),
        )
        .tooltip(|_, cx| Tooltip::view("Copy branch name".into(), cx))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(name.to_string()));
        })
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

    /// A review starting one thread per entry of `threads`: "resolved",
    /// "outdated", or anything else for one still open.
    fn review_with_threads(id: &str, state: ReviewState, threads: &[&str]) -> TimelineItem {
        let TimelineItem::Review(mut review) = review(id, state, "") else {
            unreachable!();
        };
        review.threads = threads
            .iter()
            .enumerate()
            .map(|(i, &thread)| ReviewThread {
                id: format!("{id}-thread{i}"),
                path: "src/lib.rs".into(),
                line: Some(1),
                is_resolved: thread == "resolved",
                is_outdated: thread == "outdated",
                diff_hunk: String::new(),
                comments: Vec::new(),
            })
            .collect();
        TimelineItem::Review(review)
    }

    fn comment(id: &str, is_bot: bool) -> TimelineItem {
        TimelineItem::Comment(Comment {
            id: id.into(),
            url: String::new(),
            author: Some(Actor {
                login: if is_bot { "ci[bot]" } else { "octocat" }.into(),
                name: None,
                avatar_url: None,
                is_bot,
            }),
            body: "Hello".into(),
            created_at: at(1),
            minimized: None,
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
                Entry::Quiet(run) => format!("{} quiet", run.len()),
                Entry::Folded(entries) => format!("folded [{}]", shape(entries).join(", ")),
            })
            .collect()
    }

    #[test]
    fn folds_everything_between_discussion() {
        let timeline = [
            event("d1", EventKind::Deployed { environment: None }),
            commit("c1"),
            review("approve", ReviewState::Approved, ""),
            commit("c2"),
            event("r1", EventKind::ReviewRequested { reviewer: None }),
            event("d2", EventKind::Deployed { environment: None }),
            comment("person", false),
            commit("c3"),
            review("changes", ReviewState::ChangesRequested, "Not yet"),
            event("ready", EventKind::ReadyForReview),
            comment("bot", true),
            commit("c4"),
            commit("c5"),
        ];
        let entries = timeline_entries(&timeline, &[]);
        assert_eq!(
            shape(&entries),
            [
                "folded [1 quiet, 1 commits, review approve, 1 commits, 2 quiet]",
                "comment person",
                "1 commits",
                "review changes",
                "folded [event ready, 1 quiet, 2 commits]",
            ]
        );
        let Entry::Folded(folded) = &entries[0] else {
            panic!("expected a fold");
        };
        assert_eq!(
            summarize_folded(folded),
            (
                "2 commits, 4 events".to_owned(),
                "deployments, approvals, review requests".to_owned()
            )
        );
        let Entry::Folded(folded) = &entries[4] else {
            panic!("expected a fold");
        };
        assert_eq!(
            summarize_folded(folded),
            (
                "2 commits, 2 events".to_owned(),
                "draft changes, bot comments".to_owned()
            )
        );
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
            shape(&group_timeline(&timeline, &[])),
            [
                "3 quiet",
                "2 commits",
                "event ready",
                "1 quiet",
                "review approve",
                "event merged",
            ]
        );
    }

    #[test]
    fn folds_bot_comments_into_runs_of_bookkeeping() {
        let timeline = [
            event(
                "l1",
                EventKind::Labeled {
                    label: "cap".into(),
                },
            ),
            comment("bot1", true),
            event(
                "l2",
                EventKind::Labeled {
                    label: "bug".into(),
                },
            ),
            comment("bot2", true),
            event("d1", EventKind::Deployed { environment: None }),
            commit("c1"),
            comment("bot3", true),
            comment("person", false),
            comment("bot4", true),
            event("r1", EventKind::ReviewRequested { reviewer: None }),
        ];
        let entries = group_timeline(&timeline, &[]);
        assert_eq!(
            shape(&entries),
            [
                "5 quiet",
                "1 commits",
                "1 quiet",
                "comment person",
                "2 quiet"
            ]
        );
        let Entry::Quiet(run) = &entries[0] else {
            panic!("expected a run");
        };
        assert_eq!(
            summarize_quiet(run),
            "label changes, bot comments, deployments"
        );
    }

    #[test]
    fn folds_resolved_reviews_into_runs_of_bookkeeping() {
        let timeline = [
            event("r1", EventKind::ReviewRequested { reviewer: None }),
            review_with_threads(
                "resolved",
                ReviewState::Commented,
                &["resolved", "resolved"],
            ),
            review_with_threads("outdated", ReviewState::Commented, &["outdated"]),
            review_with_threads("mixed", ReviewState::Commented, &["resolved", "outdated"]),
            review_with_threads("open", ReviewState::Commented, &["resolved", "open"]),
            review_with_threads("approved", ReviewState::Approved, &["resolved"]),
            review_with_threads("changes", ReviewState::ChangesRequested, &["outdated"]),
            review_with_threads("dismissed", ReviewState::Dismissed, &["resolved"]),
            comment("bot", true),
        ];
        let entries = group_timeline(&timeline, &[]);
        assert_eq!(
            shape(&entries),
            [
                "4 quiet",
                "review open",
                "review approved",
                "review changes",
                "2 quiet",
            ]
        );
        let Entry::Quiet(run) = &entries[0] else {
            panic!("expected a run");
        };
        assert_eq!(summarize_quiet(run), "review requests, resolved reviews");
    }

    #[test]
    fn summarizes_event_categories_in_order() {
        let timeline = [
            event("d1", EventKind::Deployed { environment: None }),
            event("r1", EventKind::ReviewRequested { reviewer: None }),
            event("d2", EventKind::Deployed { environment: None }),
        ];
        let entries = group_timeline(&timeline, &[]);
        let [Entry::Quiet(run)] = entries.as_slice() else {
            panic!("expected one group");
        };
        assert_eq!(summarize_quiet(run), "deployments, review requests");
    }

    #[test]
    fn comments_by_automated_accounts_are_noise() {
        let timeline = [comment("a", false), comment("b", true), comment("c", false)];
        assert_eq!(
            shape(&group_timeline(&timeline, &[])),
            ["comment a", "1 quiet", "comment c"]
        );
        // Logins are case-insensitive.
        let automated = ["OctoCat".to_owned()];
        assert_eq!(shape(&group_timeline(&timeline, &automated)), ["3 quiet"]);
        let entries = group_timeline(&timeline, &automated);
        let [Entry::Quiet(run)] = entries.as_slice() else {
            panic!("expected one group");
        };
        assert_eq!(summarize_quiet(run), "bot comments");
    }

    mod timeline {
        use std::sync::Arc;

        use gpui::TestAppContext;

        use super::{comment, commit, event, review_with_threads};
        use crate::github::{EventKind, FakeGithub, ReviewState, TimelineItem};
        use crate::state::SectionKind;
        use crate::test_support::{
            VisualTestContextExt as _, detail, git_checkout, open_docket, project, pull_request,
        };

        #[gpui::test]
        fn a_run_of_noise_opens_to_its_rows(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            body.timeline = vec![
                event(
                    "label",
                    EventKind::Labeled {
                        label: "cap".into(),
                    },
                ),
                comment("bot", true),
                event("deploy", EventKind::Deployed { environment: None }),
            ];
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            let (_tab, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);

            assert!(cx.is_drawn("events-label"));
            assert!(!cx.is_drawn("bot"), "the bot comment is in the run");

            cx.click_on("events-label");
            assert!(cx.is_drawn("bot"));
        }

        #[gpui::test]
        fn clicking_a_branch_copies_its_name(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), detail(&pr));
            let (_tab, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);

            cx.click_on("branch-head");
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("feature".into())
            );
            cx.click_on("branch-base");
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("main".into())
            );
        }

        #[gpui::test]
        fn everything_between_comments_folds_into_one_row(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            body.timeline = vec![
                comment("first", false),
                event(
                    "label",
                    EventKind::Labeled {
                        label: "cap".into(),
                    },
                ),
                comment("bot", true),
                commit("c1"),
                event("deploy", EventKind::Deployed { environment: None }),
                comment("second", false),
            ];
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            let (_tab, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);

            assert!(cx.is_drawn("header-first") && cx.is_drawn("header-second"));
            assert!(cx.is_drawn("folded-label"));
            assert!(!cx.is_drawn("events-label"), "the run is in the fold");

            cx.click_on("folded-label");
            assert!(cx.is_drawn("events-label"), "the run stays grouped");
            assert!(cx.is_drawn("c1"));
            assert!(!cx.is_drawn("bot"));

            cx.click_on("events-label");
            assert!(cx.is_drawn("bot"));
        }

        #[gpui::test]
        fn a_projects_automated_accounts_fold_away(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            let mut deploy = comment("deploy", false);
            if let TimelineItem::Comment(comment) = &mut deploy {
                comment.author.as_mut().unwrap().login = "Deploy-User".into();
            }
            body.timeline = vec![
                comment("first", false),
                deploy,
                event(
                    "label",
                    EventKind::Labeled {
                        label: "cap".into(),
                    },
                ),
                comment("second", false),
            ];
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            let checkout = tempfile::tempdir().unwrap();
            git_checkout(checkout.path(), &[("README.md", "# Widgets\n")]);
            let mut widgets = project(1, checkout.path(), "Acme/Widgets");
            widgets.automated_authors = vec!["deploy-user".into()];
            let (_tab, cx) = open_docket(cx, github, vec![widgets]);
            cx.click_on(&pr.id);

            assert!(cx.is_drawn("header-first") && cx.is_drawn("header-second"));
            assert!(!cx.is_drawn("header-deploy"), "the comment is noise");
            assert!(cx.is_drawn("events-deploy"), "it's in a run with the label");
            assert!(!cx.is_drawn("deploy"));

            cx.click_on("events-deploy");
            assert!(cx.is_drawn("deploy"));
        }

        #[gpui::test]
        fn a_resolved_review_folds_into_the_run(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            body.timeline = vec![
                event(
                    "label",
                    EventKind::Labeled {
                        label: "cap".into(),
                    },
                ),
                review_with_threads("review", ReviewState::Commented, &["outdated"]),
                event("deploy", EventKind::Deployed { environment: None }),
            ];
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            let (_tab, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);

            assert!(!cx.is_drawn("review"), "the review is in the run");
            cx.click_on("events-label");
            assert!(
                !cx.is_drawn("review-thread0"),
                "the review starts collapsed"
            );
            cx.click_on("review");
            assert!(cx.is_drawn("review-thread0"));
        }
    }

    mod layout {
        use std::sync::Arc;

        use gpui::{TestAppContext, px, size};

        use crate::github::FakeGithub;
        use crate::state::SectionKind;
        use crate::test_support::{VisualTestContextExt as _, detail, open_docket, pull_request};

        /// The description wraps beside the side panel at every width,
        /// including once the window is wide enough that the conversation's
        /// width is capped.
        #[gpui::test]
        fn the_description_stays_beside_the_side_panel(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            let long = "word ".repeat(200);
            body.body = format!("## What\n\n- {long}\n- {long}\n\n{long}\n");
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            let (_tab, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);

            for width in [900., 1400., 2400., 1000., 2400.] {
                cx.simulate_resize(size(px(width), px(720.)));
                cx.run_until_parked();
                let side = cx.bounds_of("side-panel").expect("the side panel is drawn");
                let text = cx
                    .bounds_of(&format!("markdown-{}", pr.id))
                    .expect("the description is drawn");
                assert!(
                    text.right() <= side.left(),
                    "at {width}px the description ({text:?}) reaches under the side panel ({side:?})"
                );
            }
        }
    }

    mod files {
        use std::sync::Arc;

        use gpui::TestAppContext;

        use crate::github::FakeGithub;
        use crate::projects::git::git;
        use crate::state::{DiffLoad, PrTab, SectionKind};
        use crate::test_support::{
            self, VisualTestContextExt as _, detail, git_checkout, git_commit, open_docket,
            project, pull_request,
        };

        /// The diff loads while the conversation is read, so the Files tab
        /// is ready when it opens.
        #[gpui::test]
        fn load_before_the_files_tab_opens(cx: &mut TestAppContext) {
            let checkout = tempfile::tempdir().unwrap();
            let root = checkout.path();
            git_checkout(root, &[("README.md", "# Widgets\n")]);
            let tip = |branch: &str| git(root, &["rev-parse", branch]).unwrap().trim().to_owned();
            test_support::git(root, &["checkout", "--quiet", "feature"]);
            std::fs::write(root.join("README.md"), "# Widgets\n\nWith sprockets.\n").unwrap();
            git_commit(root, "Add sprockets", "2026-01-02T00:00:00Z");
            test_support::git(root, &["checkout", "--quiet", "main"]);

            // Both commits are local, so nothing is fetched.
            let mut pr = pull_request("acme/widgets", 7, "Add sprockets");
            pr.base_ref_oid = tip("main");
            pr.head_ref_oid = tip("feature");
            let github = Arc::new(FakeGithub::new());
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), detail(&pr));
            let projects = vec![project(1, root, "acme/widgets")];
            let (tab, cx) = open_docket(cx, github, projects);
            cx.click_on(&pr.id);

            tab.read_with(cx, |tab, _| {
                let detail = tab.detail.as_ref().expect("a pull request is open");
                assert_eq!(detail.tab, PrTab::Conversation);
                let files = detail.files.as_ref().expect("its diff loads with it");
                let DiffLoad::Ready(diff) = &files.diff else {
                    panic!("the diff should have loaded");
                };
                let paths: Vec<_> = diff.files.iter().map(|file| file.path()).collect();
                assert_eq!(paths, ["README.md"]);
                assert!(!files.is_busy());
            });
            assert!(!cx.is_drawn("diff-file-0"));

            cx.click_on("pr-tab-files");
            assert!(cx.is_drawn("diff-file-0"));
        }
    }

    mod checks {
        use std::sync::Arc;

        use gpui::{
            Modifiers, Pixels, Point, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
            VisualTestContext, point, px,
        };

        use crate::github::{Check, CheckStatus, FakeGithub};
        use crate::state::SectionKind;
        use crate::test_support::{
            VisualTestContextExt as _, actor, detail, open_docket, pull_request,
        };

        use super::super::{CHECK_GROUP_ROWS, CHECK_ROW_HEIGHT};

        fn check(name: &str, status: CheckStatus) -> Check {
            Check {
                name: name.into(),
                status,
                url: None,
            }
        }

        /// Opens a pull request whose head has `checks`, the first page
        /// of them, with a description long enough that the page scrolls.
        fn open_with_checks(cx: &mut TestAppContext, checks: Vec<Check>) -> &mut VisualTestContext {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            for check in &checks {
                body.check_summary.add(check.status, 1);
            }
            body.checks = checks;
            body.body = "Adds sprockets.\n\n".repeat(100);
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            let (_, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);
            cx
        }

        #[gpui::test]
        fn assignees_show_only_when_someone_is_assigned(cx: &mut TestAppContext) {
            let cx = open_with_checks(cx, Vec::new());
            assert!(cx.is_drawn("side-reviewers"));
            assert!(!cx.is_drawn("side-assignees"));
        }

        #[gpui::test]
        fn assignees_show_who_is_assigned(cx: &mut TestAppContext) {
            let github = Arc::new(FakeGithub::new());
            let pr = pull_request("acme/widgets", 7, "Add sprockets");
            let mut body = detail(&pr);
            body.assignees = vec![actor("octocat")];
            github.set_search(
                SectionKind::ReviewRequested.search_query(),
                vec![pr.clone()],
            );
            github.set_detail("acme/widgets".parse().unwrap(), body);
            let (_, cx) = open_docket(cx, github, Vec::new());
            cx.click_on(&pr.id);
            assert!(cx.is_drawn("side-assignees"));
        }

        fn top_of(cx: &mut VisualTestContext, selector: &str) -> Pixels {
            cx.bounds_of(selector)
                .unwrap_or_else(|| panic!("{selector} is drawn"))
                .origin
                .y
        }

        #[gpui::test]
        fn checks_are_grouped_by_status_with_passing_collapsed(cx: &mut TestAppContext) {
            let cx = open_with_checks(
                cx,
                vec![
                    check("lint", CheckStatus::Failure),
                    check("deploy", CheckStatus::Pending),
                    check("build", CheckStatus::Success),
                    check("docs", CheckStatus::Neutral),
                ],
            );

            let groups = [
                "checks-failing",
                "checks-pending",
                "checks-successful",
                "checks-skipped",
            ]
            .map(|group| top_of(cx, group));
            assert!(groups.is_sorted(), "{groups:?}");

            assert!(cx.is_drawn("check-lint"));
            assert!(cx.is_drawn("check-deploy"));
            assert!(
                !cx.is_drawn("check-build"),
                "passing checks start collapsed"
            );
            assert!(!cx.is_drawn("check-docs"), "so do skipped ones");

            cx.click_on("checks-successful");
            assert!(cx.is_drawn("check-build"));
            cx.click_on("checks-failing");
            assert!(!cx.is_drawn("check-lint"));
        }

        #[gpui::test]
        fn a_group_scrolls_past_its_first_checks(cx: &mut TestAppContext) {
            let failing: Vec<_> = (0..CHECK_GROUP_ROWS + 3)
                .map(|n| check(&format!("test {n}"), CheckStatus::Failure))
                .collect();
            let cx = open_with_checks(
                cx,
                [failing, vec![check("deploy", CheckStatus::Pending)]].concat(),
            );

            let height = |cx: &mut VisualTestContext, selector| {
                cx.bounds_of(selector).expect("it's drawn").size.height
            };
            assert_eq!(
                height(cx, "checks-failing-list"),
                px(CHECK_ROW_HEIGHT * CHECK_GROUP_ROWS as f32)
            );
            assert_eq!(height(cx, "checks-pending-list"), px(CHECK_ROW_HEIGHT));
        }

        fn scroll(cx: &mut VisualTestContext, at: Point<Pixels>, y: f32, phase: TouchPhase) {
            cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Pixels(point(px(0.), px(y))),
                modifiers: Modifiers::none(),
                touch_phase: phase,
            });
            cx.run_until_parked();
        }

        /// Eight failing checks, which scroll, and one in progress, which
        /// doesn't.
        fn open_with_a_long_group(cx: &mut TestAppContext) -> &mut VisualTestContext {
            let failing: Vec<_> = (0..CHECK_GROUP_ROWS + 3)
                .map(|n| check(&format!("test {n}"), CheckStatus::Failure))
                .collect();
            open_with_checks(
                cx,
                [failing, vec![check("deploy", CheckStatus::Pending)]].concat(),
            )
        }

        #[gpui::test]
        fn a_gesture_on_a_group_scrolls_only_the_group(cx: &mut TestAppContext) {
            let cx = open_with_a_long_group(cx);
            let list = cx.bounds_of("checks-failing-list").unwrap().center();
            let page = top_of(cx, "checks-failing");
            let first = top_of(cx, "check-test 0");

            scroll(cx, list, 0., TouchPhase::Started);
            scroll(cx, list, -30., TouchPhase::Moved);
            assert_eq!(top_of(cx, "checks-failing"), page, "the page stays");
            assert_eq!(top_of(cx, "check-test 0"), first - px(30.));

            // Past the group's end, the gesture still doesn't move the page.
            scroll(cx, list, -1000., TouchPhase::Moved);
            assert_eq!(top_of(cx, "checks-failing"), page);
        }

        #[gpui::test]
        fn a_gesture_on_the_page_scrolls_it_past_groups(cx: &mut TestAppContext) {
            let cx = open_with_a_long_group(cx);
            let list = cx.bounds_of("checks-failing-list").unwrap().center();
            // Over the timeline, beside the side panel.
            let timeline = list - point(px(400.), px(0.));
            let page = top_of(cx, "checks-failing");

            scroll(cx, timeline, 0., TouchPhase::Started);
            scroll(cx, timeline, -30., TouchPhase::Moved);
            assert_eq!(top_of(cx, "checks-failing"), page - px(30.));

            // The group comes under the mouse, and the page keeps going.
            let in_list = |cx: &mut VisualTestContext| {
                top_of(cx, "check-test 0") - top_of(cx, "checks-failing-list")
            };
            scroll(cx, list, -30., TouchPhase::Moved);
            assert_eq!(top_of(cx, "checks-failing"), page - px(60.));
            assert_eq!(in_list(cx), px(0.), "the group stays");

            // A group that can't scroll leaves a gesture to the page.
            let pending = cx.bounds_of("checks-pending-list").unwrap().center();
            scroll(cx, pending, 0., TouchPhase::Started);
            scroll(cx, pending, -30., TouchPhase::Moved);
            assert_eq!(top_of(cx, "checks-failing"), page - px(90.));
        }
    }

    mod links {
        use std::sync::Arc;

        use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};

        use crate::OpenLink;
        use crate::github::FakeGithub;
        use crate::state::{AppEvent, AuthStatus, DiffLoad, PrTab, SectionKind, Tab};
        use crate::test_support::{
            VisualTestContextExt as _, detail, docket_windows, open_docket, pull_request,
            window_context,
        };
        use crate::ui::windows;

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

        /// GitHub with pull requests 7 and 8 of acme/widgets, neither in
        /// the docket.
        fn github() -> Arc<FakeGithub> {
            let github = Arc::new(FakeGithub::new());
            for number in [7, 8] {
                let pr = pull_request("acme/widgets", number, "Add sprockets");
                github.set_detail("acme/widgets".parse().unwrap(), detail(&pr));
            }
            github
        }

        #[gpui::test]
        fn links_from_other_apps_open_in_the_front_window(cx: &mut TestAppContext) {
            let (tab, cx) = open_docket(cx, github(), Vec::new());
            let urls = [
                "docket://github.com/acme/widgets/pull/7",
                "https://github.com/acme/widgets/issues/3",
                "https://github.com/acme/widgets/pull/8/commits",
            ];
            cx.cx
                .update(|cx| windows::open_links(urls.map(Into::into).into(), cx));
            cx.run_until_parked();

            let number = |tab: &Entity<Tab>, cx: &VisualTestContext| {
                tab.read_with(cx, |tab, _| {
                    let detail = tab.detail.as_ref().expect("a pull request is open");
                    assert!(detail.loaded.is_some(), "{:?}", detail.error);
                    (detail.number, detail.tab)
                })
            };
            assert_eq!(number(&tab, cx), (7, PrTab::Conversation));
            let opened = docket_windows(cx);
            assert_eq!(opened.len(), 2, "the second pull request is in a new tab");
            let (second, second_cx) = window_context(cx, opened[1]);
            assert_eq!(number(&second, &second_cx), (8, PrTab::Commits));
            // Not sent to the browser, which might send it right back.
            assert_eq!(cx.opened_url(), None);
        }

        #[gpui::test]
        fn a_link_from_another_app_opens_a_window(cx: &mut TestAppContext) {
            let (_, cx) = open_docket(cx, github(), Vec::new());
            cx.simulate_keystrokes("cmd-w");
            cx.run_until_parked();
            assert!(docket_windows(cx).is_empty());

            let url = "docket://github.com/acme/widgets/pull/7/files";
            cx.cx.update(|cx| windows::open_links(vec![url.into()], cx));
            let opened = docket_windows(cx);
            assert_eq!(opened.len(), 1);
            let (tab, cx) = window_context(cx, opened[0]);
            tab.read_with(&cx, |tab, _| {
                let detail = tab.detail.as_ref().expect("a pull request is open");
                assert_eq!((detail.number, detail.tab), (7, PrTab::Files));
            });
        }

        #[gpui::test]
        fn a_link_before_signing_in_opens_once_signed_in(cx: &mut TestAppContext) {
            let (tab, cx) = open_docket(cx, github(), Vec::new());
            let app = tab.read_with(cx, |tab, _| tab.app.clone());
            let signed_in = app.update(cx, |app, _| {
                std::mem::replace(&mut app.auth, AuthStatus::Restoring)
            });

            let url = "docket://github.com/acme/widgets/pull/7";
            cx.cx.update(|cx| windows::open_links(vec![url.into()], cx));
            cx.run_until_parked();
            assert!(tab.read_with(cx, |tab, _| tab.detail.is_none()));

            app.update(cx, |app, cx| {
                app.auth = signed_in;
                cx.emit(AppEvent::SignedIn);
            });
            cx.run_until_parked();
            tab.read_with(cx, |tab, _| {
                let detail = tab.detail.as_ref().expect("it opens once signed in");
                assert_eq!(detail.number, 7);
                assert!(detail.loaded.is_some(), "{:?}", detail.error);
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
