//! Lucide icons, embedded at compile time and served through GPUI's asset
//! source. To add one, copy its SVG from `lucide-static` into `assets/icons/`
//! and list it below.

use std::borrow::Cow;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, AssetSource, IntoElement, Pixels, RenderOnce, SharedString,
    Styled as _, Transformation, Window, percentage, px, svg,
};

use crate::ui::theme::{Color, Theme};

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum IconName {
            $($variant,)*
        }

        impl IconName {
            const ALL: &[IconName] = &[$(IconName::$variant,)*];

            pub fn path(self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("icons/", $file, ".svg"),)*
                }
            }

            fn svg(self) -> &'static [u8] {
                match self {
                    $(Self::$variant => include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"), "/assets/icons/", $file, ".svg"
                    )),)*
                }
            }
        }
    };
}

icons! {
    ArrowDownToLine => "arrow-down-to-line",
    ArrowLeft => "arrow-left",
    Check => "check",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    CircleCheck => "circle-check",
    CircleDashed => "circle-dashed",
    CircleDot => "circle-dot",
    CircleX => "circle-x",
    Copy => "copy",
    Ellipsis => "ellipsis",
    ExternalLink => "external-link",
    Eye => "eye",
    File => "file",
    FileCode => "file-code",
    FileDiff => "file-diff",
    FileMinus => "file-minus",
    FilePen => "file-pen",
    FilePlus => "file-plus",
    FileSymlink => "file-symlink",
    Folder => "folder",
    FolderGit => "folder-git-2",
    FolderOpen => "folder-open",
    FolderTree => "folder-tree",
    GitBranch => "git-branch",
    GitCommit => "git-commit-horizontal",
    GitMerge => "git-merge",
    GitPullRequest => "git-pull-request",
    GitPullRequestClosed => "git-pull-request-closed",
    GitPullRequestDraft => "git-pull-request-draft",
    Inbox => "inbox",
    Info => "info",
    KeyRound => "key-round",
    Layers => "layers",
    Lightbulb => "lightbulb",
    Loader => "loader-circle",
    LogIn => "log-in",
    MessageSquare => "message-square",
    MessageSquareWarning => "message-square-warning",
    OctagonAlert => "octagon-alert",
    Pin => "pin",
    Plus => "plus",
    RefreshCw => "refresh-cw",
    Settings => "settings-2",
    Square => "square",
    SquareCheck => "square-check",
    Terminal => "terminal",
    Trash => "trash-2",
    TriangleAlert => "triangle-alert",
    User => "user",
    Users => "users",
    X => "x",
}

/// The app's asset source. Icons are the only assets so far.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(IconName::ALL
            .iter()
            .find(|icon| icon.path() == path)
            .map(|icon| Cow::Borrowed(icon.svg())))
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(IconName::ALL
            .iter()
            .map(|icon| icon.path())
            .filter(|p| p.starts_with(path))
            .map(SharedString::new_static)
            .collect())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconSize {
    /// 12px, for dense metadata rows.
    XSmall,
    /// 14px, alongside body text and in buttons.
    #[default]
    Small,
    /// 16px, for navigation and list leads.
    Medium,
    /// 20px, for empty states and emphasis.
    Large,
}

impl IconSize {
    pub fn px(self) -> Pixels {
        match self {
            Self::XSmall => px(12.),
            Self::Small => px(14.),
            Self::Medium => px(16.),
            Self::Large => px(20.),
        }
    }
}

#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: IconSize,
    color: Color,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            size: IconSize::default(),
            color: Color::Muted,
        }
    }

    pub fn size(mut self, size: IconSize) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        svg()
            .flex_none()
            .size(self.size.px())
            .path(self.name.path())
            .text_color(self.color.hsla(Theme::get(cx)))
    }
}

/// An indeterminate progress indicator.
#[derive(IntoElement)]
pub struct Spinner {
    id: SharedString,
}

impl Spinner {
    /// `id` keeps the animation's state across frames; make it unique among
    /// spinners shown at once.
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self { id: id.into() }
    }
}

impl RenderOnce for Spinner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        svg()
            .flex_none()
            .size(IconSize::Small.px())
            .path(IconName::Loader.path())
            .text_color(Theme::get(cx).text_muted)
            .with_animation(
                self.id,
                Animation::new(Duration::from_millis(900)).repeat(),
                |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
            )
    }
}
