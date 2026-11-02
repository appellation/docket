//! Docket's design system components. Build views from these and the
//! tokens in `theme`, rather than styling raw `div`s with colors.

mod button;
mod icon;
mod list;

use std::path::Path;

pub use button::{Button, ButtonSize, ButtonStyle, IconButton};
pub use icon::{Assets, Icon, IconName, IconSize, Spinner};
pub use list::{Badge, Callout, CalloutKind, ListItem, count_badge, section_heading};

/// Formats a path for display, abbreviating the home directory to `~`.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}
