//! Moving, sizing and closing the window, on whatever platform is underneath.
//!
//! Windows has none of this go through here: its frame is native, the title bar
//! strip is declared non-client, and the system does the dragging, the snapping
//! and the caption buttons itself — which is what earns the Snap Layouts flyout
//! and the window animations. Slint never sees those clicks, so the callbacks
//! that land here simply never fire on Windows.
//!
//! Everywhere else there is no such frame. The window is borderless by choice,
//! and without these the title bar's buttons are decoration: the client could
//! not be minimised, maximised, moved or closed at all.

use crate::App;
use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

/// Runs `act` on the window the backend is drawing into.
fn with_window<T>(ui: &App, act: impl FnOnce(&slint::winit_030::winit::window::Window) -> T) -> Option<T> {
    ui.window().with_winit_window(act)
}

pub fn minimise(ui: &App) {
    with_window(ui, |window| window.set_minimized(true));
}

pub fn toggle_maximise(ui: &App) {
    with_window(ui, |window| {
        let maximised = window.is_maximized();
        window.set_maximized(!maximised);
        // Kept in step here rather than from a resize event, because the
        // property is what draws the button's own symbol.
        ui.set_is_maximized(!maximised);
    });
}

/// Opens the window filling the screen, which is how the client starts.
///
/// Windows reaches the same end through its frame, which has to wait for the
/// backend to finish placing the window first — see `win32_frame`.
#[cfg(not(windows))]
pub fn maximise(ui: &App) {
    open_out(ui.as_weak(), 12);
}

/// Asks, then checks, then asks again.
///
/// A single request right after `show` is ignored: the backend is still
/// applying the size it decided on from `app.slint`, and on some window
/// managers there is nothing listening yet either. Rather than guess a delay
/// long enough to cover both, this asks until the window agrees it is
/// maximised, and gives up after `left` tries so a manager that simply refuses
/// cannot spin here forever.
#[cfg(not(windows))]
fn open_out(weak: slint::Weak<App>, left: u32) {
    let Some(ui) = weak.upgrade() else { return };
    let done = with_window(&ui, |window| {
        if window.is_maximized() {
            return true;
        }
        window.set_maximized(true);
        false
    })
    .unwrap_or(false);

    if done {
        ui.set_is_maximized(true);
        return;
    }
    if left == 0 {
        return;
    }
    slint::Timer::single_shot(std::time::Duration::from_millis(40), move || {
        open_out(weak, left - 1);
    });
}

/// Hands the window to the compositor to be dragged.
///
/// Started from a press on the title bar. The window manager takes over for the
/// duration, so the movement is its own — with its own snapping and its own
/// feel — rather than something reimplemented here out of mouse deltas.
pub fn start_drag(ui: &App) {
    with_window(ui, |window| {
        // Refused when there is no press to attach to, which is not worth
        // reporting: it means the button was already up.
        let _ = window.drag_window();
    });
}
