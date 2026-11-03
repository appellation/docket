//! Scrolling lists inside a scrolling page.
//!
//! GPUI scrolls every scrollable element under the mouse by the same
//! amount, so a list that scrolls inside a page would move the page along
//! with it. `NestedScroll` sorts each scroll out the way a browser does: a
//! trackpad gesture stays with what it started on, which is a list if it
//! could move the way the gesture went and the page if not, so the page's
//! scrolling isn't caught by a list passing under the mouse. A mouse wheel's
//! clicks aren't a gesture, so each goes to the list if it can move, or
//! else the page.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    DispatchPhase, Pixels, ScrollDelta, ScrollHandle, ScrollWheelEvent, Stateful, TouchPhase,
    Window, canvas, div, point, prelude::*, px,
};

/// The lists inside one page. Clones share their state, which must outlive
/// a frame (`Window::use_keyed_state`) to keep each list's offset.
#[derive(Clone, Default)]
pub struct NestedScroll(Rc<State>);

#[derive(Default)]
struct State {
    lists: RefCell<HashMap<usize, ScrollHandle>>,
    gesture: Cell<Gesture>,
}

/// Which of the page and its lists the current trackpad gesture scrolls.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Gesture {
    /// It hasn't moved yet.
    #[default]
    Undecided,
    /// This event is its first move: a list under the mouse that can move
    /// takes it, or it goes to the page.
    Deciding,
    List(usize),
    Page,
}

impl NestedScroll {
    /// Makes `list`, which scrolls vertically, the page's list `ix`.
    pub fn list<E: StatefulInteractiveElement>(&self, ix: usize, list: E) -> E {
        let handle = self.0.lists.borrow_mut().entry(ix).or_default().clone();
        let state = self.0.clone();
        // Runs once the list has scrolled itself.
        list.track_scroll(&handle)
            .on_scroll_wheel(move |event, window, cx| {
                let delta = scroll_delta(event, window);
                let max = handle.max_offset().height;
                let clamp = |y: Pixels| y.clamp(-max, px(0.));
                let scrolled = handle.offset();
                let before = clamp(scrolled.y - delta);
                let can_move = clamp(scrolled.y) != before;
                let takes_it = match event.delta {
                    ScrollDelta::Lines(_) => can_move,
                    ScrollDelta::Pixels(_) => {
                        if state.gesture.get() == Gesture::Deciding {
                            state.gesture.set(if can_move {
                                Gesture::List(ix)
                            } else {
                                Gesture::Page
                            });
                        }
                        state.gesture.get() == Gesture::List(ix)
                    }
                };
                if takes_it {
                    cx.stop_propagation();
                } else {
                    handle.set_offset(point(scrolled.x, before));
                }
            })
    }

    /// Follows trackpad gestures across the page. Place it anywhere in the
    /// page that's drawn while its lists are.
    pub fn tracker(&self) -> Stateful<gpui::Div> {
        let state = self.0.clone();
        div().id("nested-scroll").absolute().size_0().child(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let state = state.clone();
                    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, _| {
                        if phase != DispatchPhase::Capture
                            || matches!(event.delta, ScrollDelta::Lines(_))
                        {
                            return;
                        }
                        let gesture = &state.gesture;
                        if matches!(event.touch_phase, TouchPhase::Started) {
                            gesture.set(Gesture::Undecided);
                        }
                        // No list took the last move.
                        if gesture.get() == Gesture::Deciding {
                            gesture.set(Gesture::Page);
                        }
                        if gesture.get() == Gesture::Undecided
                            && scroll_delta(event, window) != px(0.)
                        {
                            gesture.set(Gesture::Deciding);
                        }
                    });
                },
            )
            .size_full(),
        )
    }
}

/// How far `event` scrolls a vertical list, as GPUI reckons it.
fn scroll_delta(event: &ScrollWheelEvent, window: &Window) -> Pixels {
    let delta = event.delta.pixel_delta(window.line_height());
    if delta.y == px(0.) { delta.x } else { delta.y }
}
