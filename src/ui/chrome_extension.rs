//! Install Chrome Extension…: installs Docket's extension (`chrome`), then
//! shows how to load it. Chrome installs extensions on its own only from
//! its Web Store, so it's loaded unpacked from the folder Docket wrote it
//! to, which Docket keeps up to date.

use std::path::{Path, PathBuf};

use anyhow::Result;
use gpui::{App, ClipboardItem, PromptLevel, Window};

use crate::state::AppState;

const LOAD_DETAIL: &str = "Docket opens Chrome's Extensions page and shows the \
    extension's folder in Finder. Turn on Developer mode at the top right of the \
    page, then drag the folder onto it. (Or click Load unpacked, press ⌘⇧G, and \
    paste the folder's path, which Docket copies.)\n\n\
    Once it's loaded, pull requests in your docket or your projects' repositories \
    open here instead of in Chrome. Click its toolbar button to turn that off.";

/// Installs the extension, then asks to open Chrome to load it.
pub fn install(window: &mut Window, cx: &mut App) {
    let task = AppState::global(cx).update(cx, |app, cx| app.install_chrome_extension(cx));
    window
        .spawn(cx, async move |cx| {
            let result = task.await;
            cx.update(|window, cx| installed(result, window, cx)).ok();
        })
        .detach();
}

fn installed(result: Result<PathBuf>, window: &mut Window, cx: &mut App) {
    let dir = match result {
        Ok(dir) => dir,
        Err(err) => {
            // Its only answer is OK, so nothing waits for it.
            drop(window.prompt(
                PromptLevel::Critical,
                "Docket couldn't install its Chrome extension",
                Some(&format!("{err:#}")),
                &["OK"],
                cx,
            ));
            return;
        }
    };
    let answer = window.prompt(
        PromptLevel::Info,
        "Load Docket's extension in Chrome",
        Some(LOAD_DETAIL),
        &["Open Chrome", "Cancel"],
        cx,
    );
    cx.spawn(async move |cx| {
        if answer.await == Ok(0) {
            cx.update(|cx| show(&dir, cx)).ok();
        }
    })
    .detach();
}

/// Copies the extension's folder's path, shows it in Finder, and opens
/// Chrome's Extensions page, which only Chrome can open.
fn show(dir: &Path, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(dir.display().to_string()));
    if cfg!(test) {
        return;
    }
    cx.reveal_path(dir);
    cx.background_executor()
        .spawn(async {
            let opened = std::process::Command::new("open")
                .args(["-a", "Google Chrome", "chrome://extensions"])
                .status();
            if !opened.is_ok_and(|status| status.success()) {
                eprintln!("couldn't open Chrome's Extensions page");
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::TestAppContext;

    use crate::InstallChromeExtension;
    use crate::chrome::{EXTENSION_ID, Install};
    use crate::github::FakeGithub;
    use crate::state::{AppState, SectionKind};
    use crate::test_support::{git_checkout, open_docket, project, pull_request};

    #[gpui::test]
    fn installs_the_extension_from_the_menu(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (_tab, cx) = open_docket(cx, Arc::new(FakeGithub::new()), vec![]);
        let install = Install {
            extension_dir: dir.path().join("dev.docket/Chrome Extension"),
            support_dir: dir.path().to_owned(),
            executable: "/Applications/Docket.app/Contents/MacOS/docket".into(),
        };
        cx.update(|_, cx| {
            AppState::global(cx).update(cx, |app, _| app.set_extension_install(install.clone()))
        });

        cx.dispatch_action(InstallChromeExtension);
        cx.run_until_parked();
        assert!(install.is_installed());
        let host = std::fs::read_to_string(
            dir.path()
                .join("Google/Chrome/NativeMessagingHosts/dev.docket.json"),
        )
        .unwrap();
        assert!(host.contains(EXTENSION_ID));

        assert!(cx.has_pending_prompt(), "says how to load it");
        cx.simulate_prompt_answer("Open Chrome");
        cx.run_until_parked();
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(install.extension_dir.display().to_string()),
            "copies the folder for Load unpacked"
        );
    }

    #[gpui::test]
    fn says_when_it_cant_install(cx: &mut TestAppContext) {
        let (_tab, cx) = open_docket(cx, Arc::new(FakeGithub::new()), vec![]);
        cx.dispatch_action(InstallChromeExtension);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
    }

    #[gpui::test]
    fn opens_the_docket_and_projects_repositories(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        git_checkout(dir.path(), &[("README.md", "# Widgets\n")]);
        let github = Arc::new(FakeGithub::new());
        let pr = pull_request("other/thing", 3, "Elsewhere");
        github.set_search(SectionKind::ReviewRequested.search_query(), vec![pr]);
        let (tab, cx) = open_docket(cx, github, vec![project(1, dir.path(), "Acme/Widgets")]);
        cx.run_until_parked();
        let links = tab.read_with(cx, |tab, cx| tab.app.read(cx).open_links());
        assert_eq!(links.repositories, ["acme/widgets"]);
        assert_eq!(links.pull_requests, ["other/thing#3"]);
    }
}
