//! Docket's design system components. Build views from these and the
//! tokens in `theme`, rather than styling raw `div`s with colors.

mod button;
mod icon;
mod list;
mod markdown;
mod nested_scroll;
mod tabs;
mod tree;

use std::path::Path;

use chrono::{DateTime, Utc};
use gpui::InteractiveElement;

pub use button::{Button, ButtonSize, ButtonStyle, IconButton, Tooltip};
pub use icon::{Assets, Icon, IconName, IconSize, Spinner};
pub use list::{Badge, Callout, CalloutKind, ListItem, count_badge, section_heading};
pub use markdown::Markdown;
pub use nested_scroll::NestedScroll;
pub use tabs::{TabBar, TabBarItem};
pub use tree::TreeItem;

/// The font for code, branch names, and commit hashes.
pub const MONOSPACE: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "monospace"
};

/// Names an element so UI tests can find it (see `test_support`). Costs
/// nothing outside tests: GPUI never calls `selector` there.
pub trait TestSelector: InteractiveElement + Sized {
    fn test_selector(self, selector: impl FnOnce() -> String) -> Self {
        self.debug_selector(|| {
            let selector = selector();
            #[cfg(test)]
            crate::test_support::note_drawn(&selector);
            selector
        })
    }
}

impl<E: InteractiveElement> TestSelector for E {}

/// Formats a path for display, abbreviating the home directory to `~`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}

/// How long ago `at` was, as GitHub phrases it: "3 hours ago", then a date
/// once it's more than a month old.
pub fn relative_time(at: DateTime<Utc>) -> String {
    relative_to(at, Utc::now())
}

fn relative_to(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let elapsed = now.signed_duration_since(at);
    let ago = |n: i64, unit: &str| match n {
        1 => format!("1 {unit} ago"),
        n => format!("{n} {unit}s ago"),
    };
    match elapsed.num_seconds() {
        ..60 => "just now".into(),
        60..3600 => ago(elapsed.num_minutes(), "minute"),
        3600..86_400 => ago(elapsed.num_hours(), "hour"),
        86_400..2_592_000 => match elapsed.num_days() {
            1 => "yesterday".into(),
            n => ago(n, "day"),
        },
        _ if at.format("%Y").to_string() == now.format("%Y").to_string() => {
            at.format("%b %-d").to_string()
        }
        _ => at.format("%b %-d, %Y").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn formats_relative_times() {
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        let before = |secs: i64| relative_to(now - chrono::Duration::seconds(secs), now);
        assert_eq!(before(30), "just now");
        assert_eq!(before(60), "1 minute ago");
        assert_eq!(before(3 * 3600), "3 hours ago");
        assert_eq!(before(86_400 + 10), "yesterday");
        assert_eq!(before(5 * 86_400), "5 days ago");
        assert_eq!(before(60 * 86_400), "Sep 3");
        assert_eq!(before(400 * 86_400), "Sep 28, 2025");
    }
}
