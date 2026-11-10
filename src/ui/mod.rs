mod branch_view;
mod chrome_extension;
mod code;
mod commit_list;
mod components;
mod diff_view;
mod file_browser;
mod pr_actions;
mod pr_detail;
mod pr_list;
mod project_setup;
mod sidebar;
mod sidebar_stack;
mod sign_in;
pub mod text_input;
pub mod theme;
pub mod windows;
mod workspace;

use gpui::App;

pub use components::Assets;
pub use diff_view::DiffView;
pub use file_browser::FileBrowserView;
pub use workspace::Workspace;

/// Opens `url` in the browser. A pull request is marked to stay there
/// (`github::browser_url`), so Docket's Chrome extension doesn't send it
/// straight back.
pub fn open_in_browser(url: &str, cx: &App) {
    cx.open_url(&crate::github::browser_url(url));
}
