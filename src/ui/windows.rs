//! Docket's windows, each a [`Workspace`] with a [`Tab`] of its own, so
//! they're independent: what one shows, and its language servers, don't
//! change with the others.
//!
//! On macOS windows are native tabs. They share a tabbing identifier, so
//! AppKit gives them its tab bar and its Window menu items (Show All Tabs,
//! Merge All Windows, Move Tab to New Window, Show Next Tab), and New
//! Window follows the user's "Prefer tabs when opening documents" setting.
//! New Tab, and opening a definition in a new tab, always add one to the
//! window it came from; GPUI leaves that to the setting, so that's asked
//! of AppKit directly (`native`).
//!
//! GPUI hides AppKit's tab bar, since Zed draws its own; [`init`] puts it
//! back, with its "+" button for a new tab.

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, TitlebarOptions, WindowBounds,
    WindowHandle, WindowOptions, point, px, size,
};

use super::Workspace;
use crate::state::{AppState, SectionKind, Tab};
use crate::ui::theme::Theme;

/// Windows with the same identifier can be tabs of one another.
const TABBING_IDENTIFIER: &str = "docket";
/// How far a new window sits from the one it opened from.
const CASCADE: f32 = 24.;

/// Shows AppKit's tab bar on Docket's windows, with its "+" button. Before
/// any window opens.
pub fn init(cx: &mut App) {
    native::show_tab_bar();
    native::add_new_tab_button(cx);
}

/// A new window, as its own window or as a tab, depending on the user's
/// settings. It starts like the active window: the same inbox and project.
pub fn new_window(cx: &mut App) {
    open_later(false, cx);
}

/// A new tab in the active window, showing the same inbox and project.
pub fn new_tab(cx: &mut App) {
    open_later(true, cx);
}

/// Opens from the active window once the action that asked is done: while
/// it's dispatched, its window can't be read or updated.
fn open_later(as_tab: bool, cx: &mut App) {
    let from = cx.active_window();
    cx.defer(move |cx| {
        open(from, as_tab, cx, |_, _| {});
    });
}

/// Opens a window (or, with `as_tab`, a tab of `from`) whose tab starts
/// like `from`'s, then has `init` set it up. Without `from`, it shows the
/// review requests for the last project selected. Not while `from` is
/// being updated, e.g. from its action handlers: see [`open_later`].
pub fn open(
    from: Option<AnyWindowHandle>,
    as_tab: bool,
    cx: &mut App,
    init: impl FnOnce(&mut Tab, &mut Context<Tab>) + 'static,
) -> Option<WindowHandle<Workspace>> {
    let app = AppState::global(cx);
    let from = from.and_then(|handle| handle.downcast::<Workspace>());
    let like = from
        .and_then(|handle| handle.read(cx).ok())
        .map(|workspace| {
            let tab = workspace.tab().read(cx);
            (tab.selected, tab.selected_project)
        });
    let (selected, project) =
        like.unwrap_or((SectionKind::ReviewRequested, app.read(cx).last_project));
    let from_bounds = from.and_then(|handle| {
        handle
            .update(cx, |_, window, _| window.window_bounds())
            .ok()
    });
    let bounds = match from_bounds {
        // A tab takes its window's frame; AppKit sizes it to match anyway.
        Some(bounds) if as_tab => bounds,
        Some(WindowBounds::Windowed(bounds)) => WindowBounds::Windowed(Bounds {
            origin: bounds.origin + point(px(CASCADE), px(CASCADE)),
            size: bounds.size,
        }),
        _ => WindowBounds::Windowed(Bounds::centered(None, size(px(1100.), px(720.)), cx)),
    };
    let joins = as_tab && from.is_some();
    let options = WindowOptions {
        window_bounds: Some(bounds),
        titlebar: Some(TitlebarOptions {
            title: Some("Docket".into()),
            ..Default::default()
        }),
        // A tab shows once it's in the tab bar, not as a window first.
        show: !(joins && native::CAN_ADD_TABS),
        tabbing_identifier: Some(TABBING_IDENTIFIER.into()),
        ..Default::default()
    };
    let handle = cx.open_window(options, |window, cx| {
        window
            .observe_window_appearance(|_, cx| Theme::sync_system_appearance(cx))
            .detach();
        let tab = cx.new(|cx| {
            let mut tab = Tab::new(app, selected, project, cx);
            init(&mut tab, cx);
            tab
        });
        cx.new(|cx| Workspace::new(tab, window, cx))
    });
    let handle = match handle {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("failed to open a window: {err:#}");
            return None;
        }
    };
    if let (true, Some(from)) = (joins, from) {
        native::add_tab(from.into(), handle.into(), cx);
    }
    handle
        .update(cx, |_, window, _| window.activate_window())
        .ok();
    Some(handle)
}

#[cfg(all(target_os = "macos", not(test)))]
mod native {
    use std::sync::OnceLock;

    use futures::StreamExt as _;
    use futures::channel::mpsc::{self, UnboundedSender};
    use gpui::{AnyWindowHandle, App, SystemWindowTabController, Window};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;
    use objc2_app_kit::{NSView, NSWindow, NSWindowOrderingMode};
    use raw_window_handle::RawWindowHandle;

    pub const CAN_ADD_TABS: bool = true;

    /// GPUI's window class overrides `addTitlebarAccessoryViewController:`
    /// to hide the tab bar (an accessory AppKit adds when a window has
    /// tabs) and give it no height. Its classes are registered at launch,
    /// so this points the override back at `NSWindow`'s own.
    pub fn show_tab_bar() {
        let (Some(gpui), Some(appkit)) = (AnyClass::get(c"GPUIWindow"), AnyClass::get(c"NSWindow"))
        else {
            eprintln!("couldn't find GPUI's window class to show the tab bar");
            return;
        };
        let sel = sel!(addTitlebarAccessoryViewController:);
        let (Some(override_), Some(original)) =
            (gpui.instance_method(sel), appkit.instance_method(sel))
        else {
            return;
        };
        // SAFETY: the same selector, so the same signature, and NSWindow's
        // own implementation is what GPUI's calls before hiding the view.
        unsafe { override_.set_implementation(original.implementation()) };
    }

    /// The windows whose "+" was clicked, by `NSWindow` address, for the
    /// app to open a tab from. AppKit calls in without GPUI's app at hand.
    static PLUS_CLICKS: OnceLock<UnboundedSender<usize>> = OnceLock::new();

    /// The tab bar has a "+" button when its window answers
    /// `newWindowForTab:`. GPUI's window class doesn't, so this adds it.
    pub fn add_new_tab_button(cx: &mut App) {
        let Some(gpui) = AnyClass::get(c"GPUIWindow") else {
            return;
        };
        let (clicks, mut clicked) = mpsc::unbounded();
        if PLUS_CLICKS.set(clicks).is_err() {
            return;
        }
        unsafe extern "C-unwind" fn new_window_for_tab(
            this: &AnyObject,
            _: Sel,
            _sender: *mut AnyObject,
        ) {
            if let Some(clicks) = PLUS_CLICKS.get() {
                clicks
                    .unbounded_send(this as *const AnyObject as usize)
                    .ok();
            }
        }
        // SAFETY: `newWindowForTab:` takes the sender and returns nothing,
        // which is `v@:@` and the signature above, and GPUI's class doesn't
        // implement it, so nothing is replaced.
        let added = unsafe {
            let imp: Imp = std::mem::transmute(
                new_window_for_tab as unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject),
            );
            objc2::ffi::class_addMethod(
                gpui as *const AnyClass as *mut AnyClass,
                sel!(newWindowForTab:),
                imp,
                c"v@:@".as_ptr(),
            )
        };
        if !added.as_bool() {
            eprintln!("couldn't add the tab bar's new tab button");
            return;
        }
        cx.spawn(async move |cx| {
            while let Some(window) = clicked.next().await {
                cx.update(|cx| {
                    let from = gpui_window(window, cx);
                    super::open(from, true, cx, |_, _| {});
                })
                .ok();
            }
        })
        .detach();
    }

    /// The GPUI window whose `NSWindow` is at `address`.
    fn gpui_window(address: usize, cx: &mut App) -> Option<AnyWindowHandle> {
        cx.windows().into_iter().find(|handle| {
            handle
                .update(cx, |_, window, _| {
                    ns_window(window).is_some_and(|w| Retained::as_ptr(&w) as usize == address)
                })
                .unwrap_or(false)
        })
    }

    /// Adds `tab` to `host`'s tab group, after it, unless it's already
    /// there (the user prefers tabs always, so GPUI added it).
    pub fn add_tab(host: AnyWindowHandle, tab: AnyWindowHandle, cx: &mut App) {
        let Some(host) = host
            .update(cx, |_, window, _| ns_window(window))
            .ok()
            .flatten()
        else {
            return;
        };
        tab.update(cx, |_, window, cx| {
            let Some(added) = ns_window(window) else {
                return;
            };
            let group = |window: &NSWindow| window.tabGroup().map(|g| Retained::as_ptr(&g));
            if group(&host) != group(&added) {
                host.addTabbedWindow_ordered(&added, NSWindowOrderingMode::Above);
            }
            // GPUI tracks tab groups itself, for Show Next Tab and the
            // like. It saw this window open alone.
            let id = gpui::Window::window_handle(window).window_id();
            SystemWindowTabController::remove_tab(cx, id);
            if let Some(tabs) = window.tabbed_windows() {
                SystemWindowTabController::add_tab(cx, id, tabs);
            }
        })
        .ok();
    }

    fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        // SAFETY: GPUI's handle is its window's content view, which lives
        // as long as the window, and this runs on the main thread.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        view.window()
    }
}

/// Elsewhere (and in tests, whose windows aren't real) a tab is a window.
#[cfg(not(all(target_os = "macos", not(test))))]
mod native {
    use gpui::{AnyWindowHandle, App};

    pub const CAN_ADD_TABS: bool = false;

    pub fn show_tab_bar() {}

    pub fn add_new_tab_button(_cx: &mut App) {}

    pub fn add_tab(_host: AnyWindowHandle, _tab: AnyWindowHandle, _cx: &mut App) {}
}
