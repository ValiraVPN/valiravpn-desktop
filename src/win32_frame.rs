//! Custom title bar that KEEPS the native Windows frame.
//!
//! The window is created undecorated (`no-frame`), then its `HWND` is
//! subclassed to:
//!   * `WM_NCCALCSIZE`  → extend the client over the OS caption (hiding it)
//!                        while keeping the resize frame.
//!   * `WM_NCHITTEST`   → resize borders + an `HTCAPTION` drag band + the three
//!                        caption buttons (`HTMIN/HTMAX/HTCLOSE`), so Windows 11
//!                        Snap Layouts appear on hover.
//!   * `WM_NCLBUTTONUP` → minimise / maximise-restore / close.
//!   * `WM_NCMOUSEMOVE`/`WM_NCMOUSELEAVE` → mirrors the hovered button into the
//!                        Slint `caption-hover` property so our buttons light up.
//!   * `WM_SIZE`        → mirrors the maximised state into `is-maximized`.
//!
//! The subclass proc runs on the Slint UI thread, so it calls the Slint setters
//! directly.
//!
//! The acrylic backdrop is only installed when the window is actually
//! transparent, which requires the GPU renderer. Under the software rasteriser
//! the window is opaque and blurring behind it would show nothing.

use core::cell::Cell;
use core::ffi::c_void;

use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

use windows::core::{s, w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, RedrawWindow, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, RDW_ALLCHILDREN, RDW_FRAME, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, Shell_NotifyIconW, NIF_ICON,
    NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, IsZoomed, PostMessageW, SetWindowPos, ShowWindow, HTBOTTOM,
    HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTCLOSE, HTLEFT, HTMAXBUTTON, HTMINBUTTON,
    HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, NCCALCSIZE_PARAMS, SM_CXFRAME, SM_CXPADDEDBORDER,
    SM_CYFRAME, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE,
    SW_RESTORE, WM_CLOSE, WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDOWN,
    WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_SIZE, WM_WINDOWPOSCHANGED,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, GWL_STYLE, MINMAXINFO, STYLESTRUCT, WM_GETMINMAXINFO,
    WM_NCACTIVATE, WM_STYLECHANGING, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU,
    WS_THICKFRAME,
};
use windows::Win32::UI::WindowsAndMessaging::{SWP_NOCOPYBITS, WINDOWPOS, WM_WINDOWPOSCHANGING};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR,
    SM_CXICON, SM_CXSMICON, SM_CYSMICON, WM_SETICON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IsIconic, RegisterWindowMessageW,
    SetForegroundWindow, TrackPopupMenu, HICON, MF_SEPARATOR, MF_STRING, SW_HIDE, SW_SHOW,
    TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_ENDSESSION,
    WM_LBUTTONUP, WM_QUERYENDSESSION, WM_RBUTTONUP,
};
use windows::Win32::UI::WindowsAndMessaging::{IsWindowVisible, SC_MAXIMIZE, WM_SYSCOMMAND};

// Undocumented themed caption/frame paints (composition-off path).
const WM_NCUAHDRAWCAPTION: u32 = 0x00AE;
const WM_NCUAHDRAWFRAME: u32 = 0x00AF;

/// What the tray icon sends us. Anything from WM_APP up is ours to define.
const WM_TRAY: u32 = WM_APP + 1;
/// Asks a running client to shut down properly, tunnel and all. Sent by
/// `valira-desktop --quit`, which is how the installer clears the way before
/// replacing a binary the tray is still holding open.
const WM_QUIT_REQUEST: u32 = WM_APP + 2;
/// The one icon we register, in this window's namespace.
const TRAY_ID: u32 = 1;
const MENU_OPEN: usize = 1;
const MENU_QUIT: usize = 2;
// WVR_HREDRAW | WVR_VREDRAW — forces a full redraw on resize.
const WVR_REDRAW: u32 = 0x0300;

// Logical pixels. The bar's height must match `theme.slint`, and the button
// width the CaptionButton in `components/window-controls.slint` — both are
// fixed by choice. Everything else the hit-test needs is measured after layout
// and read back through `hit-*` properties, so a control that sizes to its own
// content cannot drift out of step with the frame around it.
const TITLEBAR_H: f32 = 44.0;
const BTN_W: f32 = 46.0;
const SUBCLASS_ID: usize = 1;

struct Ctx {
    weak: slint::Weak<crate::App>,
    acrylic: bool,
    tracking: Cell<bool>,
    last_client_w: Cell<i32>,
    last_client_h: Cell<i32>,
    /// Set until the window has been opened out to fill the screen. See where
    /// it is cleared, in `WM_WINDOWPOSCHANGED`.
    open_maximised: Cell<bool>,
}

/// HWND of the winit window, or `None` if it does not exist yet.
fn window_hwnd(ui: &crate::App) -> Option<HWND> {
    ui.window()
        .with_winit_window(|w| {
            use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            match w.window_handle() {
                Ok(h) => match h.as_raw() {
                    RawWindowHandle::Win32(x) => Some(HWND(x.hwnd.get() as *mut c_void)),
                    _ => None,
                },
                Err(_) => None,
            }
        })
        .flatten()
}

/// Installs the custom frame then reveals the window (created hidden). Retries
/// while the winit window does not exist yet.
///
/// `on_error` runs when the window refuses to appear. That is where a missing
/// OpenGL driver finally shows up: the renderer is built suspended and only
/// creates its context here.
pub fn install_and_reveal(ui: &crate::App, acrylic: bool, on_error: fn()) {
    if window_hwnd(ui).is_none() {
        let weak = ui.as_weak();
        slint::Timer::single_shot(core::time::Duration::from_millis(8), move || {
            if let Some(ui) = weak.upgrade() {
                install_and_reveal(&ui, acrylic, on_error);
            }
        });
        return;
    }
    install(ui, acrylic);
    // Asked before the window is put up, so it comes up already filling the
    // screen instead of appearing at its preferred size and jumping.
    ui.window().set_maximized(true);
    if ui.show().is_err() || crate::force_render_failure() {
        on_error();
        return;
    }

}

/// Installs the custom frame subclass. The window may still be hidden.
pub fn install(ui: &crate::App, acrylic: bool) {
    let Some(hwnd) = window_hwnd(ui) else {
        return;
    };

    let ctx = Box::into_raw(Box::new(Ctx {
        weak: ui.as_weak(),
        acrylic,
        tracking: Cell::new(false),
        last_client_w: Cell::new(0),
        last_client_h: Cell::new(0),
        open_maximised: Cell::new(true),
    }));

    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, ctx as usize);

        // Created undecorated: add back ONLY the wanted behaviours — a sizing
        // frame (resize + Aero Snap + animation), a maximize box (Snap Layouts
        // through HTMAXBUTTON), a minimize box / system menu — while making
        // sure WS_CAPTION never comes back.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let new_style =
            (style | WS_THICKFRAME.0 | WS_MAXIMIZEBOX.0 | WS_MINIMIZEBOX.0 | WS_SYSMENU.0)
                & !WS_CAPTION.0;
        SetWindowLongPtrW(hwnd, GWL_STYLE, new_style as isize);

        apply_window_icon(hwnd);
        ensure_tray_icon(ui, 10);
        apply_backdrop(hwnd, acrylic);
        refresh_backdrop_size(hwnd, &*ctx);

        // Rounded corners on Windows 11 (ignored on Windows 10).
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const c_void,
            core::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );

        // Recompute the non-client area now that our handler is in place.
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );

        if let Some(ui) = (*ctx).weak.upgrade() {
            ui.set_is_maximized(IsZoomed(hwnd).as_bool());
        }
    }
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    umsg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    refdata: usize,
) -> LRESULT {
    unsafe {
        let ctx = &*(refdata as *const Ctx);

        match umsg {
            // Removes the standard caption, keeps the resize frame.
            WM_NCCALCSIZE if wparam.0 != 0 => {
                // Maximised: pin the client to the monitor work area (taskbar
                // preserved, no 1-2px overhang).
                if IsZoomed(hwnd).as_bool() {
                    let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
                    let hmon = MonitorFromRect(&params.rgrc[0], MONITOR_DEFAULTTONEAREST);
                    let mut mi = MONITORINFO {
                        cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };
                    if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                        params.rgrc[0] = mi.rcWork;
                    }
                }
                LRESULT(WVR_REDRAW as isize)
            }

            WM_NCHITTEST => {
                let sx = (lparam.0 & 0xFFFF) as i16 as i32;
                let sy = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;

                let mut rc = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rc);
                let dpi = GetDpiForWindow(hwnd);
                let scale = dpi as f32 / 96.0;
                let maximized = IsZoomed(hwnd).as_bool();

                if !maximized {
                    let bx = GetSystemMetricsForDpi(SM_CXFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                    let by = GetSystemMetricsForDpi(SM_CYFRAME, dpi)
                        + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
                    let l = sx < rc.left + bx;
                    let r = sx >= rc.right - bx;
                    let t = sy < rc.top + by;
                    let b = sy >= rc.bottom - by;
                    if t && l {
                        return LRESULT(HTTOPLEFT as isize);
                    }
                    if t && r {
                        return LRESULT(HTTOPRIGHT as isize);
                    }
                    if b && l {
                        return LRESULT(HTBOTTOMLEFT as isize);
                    }
                    if b && r {
                        return LRESULT(HTBOTTOMRIGHT as isize);
                    }
                    if l {
                        return LRESULT(HTLEFT as isize);
                    }
                    if r {
                        return LRESULT(HTRIGHT as isize);
                    }
                    if t {
                        return LRESULT(HTTOP as isize);
                    }
                    if b {
                        return LRESULT(HTBOTTOM as isize);
                    }
                }

                // Title bar band (client origin → accounts for the maximised inset).
                let mut org = POINT { x: 0, y: 0 };
                let _ = ClientToScreen(hwnd, &mut org);
                let mut crc = RECT::default();
                let _ = GetClientRect(hwnd, &mut crc);
                let title_h = (TITLEBAR_H * scale) as i32;
                if sy >= org.y && sy < org.y + title_h {
                    let btn_w = (BTN_W * scale) as i32;
                    let from_left = sx - org.x;
                    let from_right = (org.x + crc.right) - sx;

                    // The three caption buttons keep their own hit codes, which
                    // is what gives Windows 11 its Snap Layouts on hover.
                    if from_right >= 0 && from_right < btn_w * 3 {
                        if from_right < btn_w {
                            return LRESULT(HTCLOSE as isize);
                        } else if from_right < btn_w * 2 {
                            return LRESULT(HTMAXBUTTON as isize);
                        } else {
                            return LRESULT(HTMINBUTTON as isize);
                        }
                    }

                    // Everything else in the bar drags the window, except the
                    // two controls Slint draws. Their extents are measured over
                    // there and read here: the tunnel chip is as wide as its
                    // label, so no constant could describe it.
                    if let Some(ui) = ctx.weak.upgrade() {
                        let px = |logical: f32| (logical * scale) as i32;

                        let chip_start = px(ui.get_hit_chip_start());
                        let chip_end = px(ui.get_hit_chip_end());
                        if chip_end > chip_start
                            && from_left >= chip_start
                            && from_left < chip_end
                        {
                            return LRESULT(HTCLIENT as isize);
                        }

                        let profile_w = px(ui.get_hit_profile_width());
                        if profile_w > 0
                            && from_right >= btn_w * 3
                            && from_right < btn_w * 3 + profile_w
                        {
                            return LRESULT(HTCLIENT as isize);
                        }
                    }
                    return LRESULT(HTCAPTION as isize);
                }
                LRESULT(HTCLIENT as isize)
            }

            WM_NCMOUSEMOVE => {
                let n = match wparam.0 as i32 {
                    x if x == HTMINBUTTON as i32 => 1,
                    x if x == HTMAXBUTTON as i32 => 2,
                    x if x == HTCLOSE as i32 => 3,
                    _ => 0,
                };
                if let Some(ui) = ctx.weak.upgrade() {
                    ui.set_caption_hover(n);
                }
                if n != 0 && !ctx.tracking.get() {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: core::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE | TME_NONCLIENT,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    ctx.tracking.set(true);
                }
                DefSubclassProc(hwnd, umsg, wparam, lparam)
            }

            WM_NCMOUSELEAVE => {
                ctx.tracking.set(false);
                if let Some(ui) = ctx.weak.upgrade() {
                    ui.set_caption_hover(0);
                }
                DefSubclassProc(hwnd, umsg, wparam, lparam)
            }

            // Swallow presses on our buttons so Windows does not draw its own.
            WM_NCLBUTTONDOWN => {
                let h = wparam.0 as i32;
                if h == HTMINBUTTON as i32 || h == HTMAXBUTTON as i32 || h == HTCLOSE as i32 {
                    return LRESULT(0);
                }
                DefSubclassProc(hwnd, umsg, wparam, lparam)
            }

            WM_NCLBUTTONUP => {
                let h = wparam.0 as i32;
                if h == HTMINBUTTON as i32 {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                    return LRESULT(0);
                } else if h == HTMAXBUTTON as i32 {
                    if IsZoomed(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    } else {
                        let _ = ShowWindow(hwnd, SW_MAXIMIZE);
                    }
                    return LRESULT(0);
                } else if h == HTCLOSE as i32 {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    return LRESULT(0);
                }
                DefSubclassProc(hwnd, umsg, wparam, lparam)
            }

            WM_SIZE => {
                if let Some(ui) = ctx.weak.upgrade() {
                    ui.set_is_maximized(IsZoomed(hwnd).as_bool());
                }
                refresh_backdrop_size(hwnd, ctx);
                DefSubclassProc(hwnd, umsg, wparam, lparam)
            }

            // Pin the maximised window to the work area (never covers the taskbar).
            WM_GETMINMAXINFO => {
                let res = DefSubclassProc(hwnd, umsg, wparam, lparam);
                let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO {
                    cbSize: core::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if GetMonitorInfoW(hmon, &mut mi).as_bool() {
                    let work = mi.rcWork;
                    let mon = mi.rcMonitor;
                    let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
                    mmi.ptMaxPosition.x = work.left - mon.left;
                    mmi.ptMaxPosition.y = work.top - mon.top;
                    mmi.ptMaxSize.x = work.right - work.left;
                    mmi.ptMaxSize.y = work.bottom - work.top;
                }
                res
            }

            // STYLE LOCK: winit reapplies its flags (re-adding WS_CAPTION,
            // dropping WS_THICKFRAME) on maximize/restore/resize. Veto that.
            WM_STYLECHANGING if wparam.0 as i32 == GWL_STYLE.0 => {
                let ss = &mut *(lparam.0 as *mut STYLESTRUCT);
                ss.styleNew = (ss.styleNew
                    | WS_THICKFRAME.0
                    | WS_MAXIMIZEBOX.0
                    | WS_MINIMIZEBOX.0
                    | WS_SYSMENU.0)
                    & !WS_CAPTION.0;
                LRESULT(0)
            }

            // Stops DefWindowProc repainting the native caption on focus change.
            WM_NCACTIVATE => DefSubclassProc(hwnd, umsg, wparam, LPARAM(-1)),

            // Swallow the undocumented themed caption/frame paints.
            WM_NCUAHDRAWCAPTION | WM_NCUAHDRAWFRAME => LRESULT(0),

            // Suppress the BitBlt of stale bits during live resize.
            WM_WINDOWPOSCHANGING => {
                let wp = &mut *(lparam.0 as *mut WINDOWPOS);
                if (wp.flags.0 & SWP_NOSIZE.0) == 0 {
                    wp.flags = wp.flags | SWP_NOCOPYBITS;
                }
                DefSubclassProc(hwnd, umsg, wparam, lparam)
            }

            WM_WINDOWPOSCHANGED => {
                let res = DefSubclassProc(hwnd, umsg, wparam, lparam);
                let wp = &*(lparam.0 as *const WINDOWPOS);
                if (wp.flags.0 & SWP_NOSIZE.0) == 0 {
                    refresh_backdrop_size(hwnd, ctx);

                    // The window opens filling the screen, and this is where
                    // that is asked for: on the first size the backend gives
                    // it, never again.
                    //
                    // Not earlier. A `ShowWindow(SW_MAXIMIZE)` straight after
                    // `show` did set the maximised style, but the backend then
                    // applied the preferred size from `app.slint` over the top
                    // — leaving a window that called itself maximised while
                    // sitting at a third of the screen. Waiting for that first
                    // placement means there is nothing left to undo it.
                    if ctx.open_maximised.replace(false) && IsWindowVisible(hwnd).as_bool() {
                        let _ = PostMessageW(
                            Some(hwnd),
                            WM_SYSCOMMAND,
                            WPARAM(SC_MAXIMIZE as usize),
                            LPARAM(0),
                        );
                    }
                }
                res
            }

            // The close button hides the window; it does not end the program and
            // it does not touch the tunnel. Quitting is "Close" in the tray menu.
            WM_CLOSE => {
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }

            // Windows shutting down, logging off, or an installer asking the
            // Restart Manager to clear the way. `WM_CLOSE` only hides this
            // window — that is what the tray icon is for — so without these the
            // process was killed where it stood, and the tunnel it had brought
            // up outlived it along with every route pinned for it.
            WM_QUERYENDSESSION => LRESULT(1),

            WM_ENDSESSION => {
                // Only when the session really is ending: a cancelled shutdown
                // sends this too, with FALSE.
                if wparam.0 != 0 {
                    remove_tray_icon(hwnd);
                    // Taken down here rather than left to the exit path, since
                    // there is no guarantee of getting back to it: Windows
                    // allows a handful of seconds and then terminates. The
                    // teardown itself takes a fraction of one.
                    crate::tunnel::release_on_exit();
                    let _ = slint::quit_event_loop();
                }
                LRESULT(0)
            }

            // Asked by name, so a running client can be shut down properly from
            // outside — the installer does this before replacing the binary.
            WM_QUIT_REQUEST => {
                remove_tray_icon(hwnd);
                let _ = slint::quit_event_loop();
                LRESULT(0)
            }

            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP => reveal(hwnd),
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_tray_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }

            // Explorer restarted and every tray icon went with it. Without this
            // the client would keep running with no way back to its window.
            _ if umsg != 0 && umsg == taskbar_created() => {
                add_tray_icon(hwnd);
                DefSubclassProc(hwnd, umsg, wparam, lparam)
            }

            WM_COMMAND => {
                match (wparam.0 & 0xFFFF) as usize {
                    MENU_OPEN => reveal(hwnd),
                    // The event loop returns from `run_event_loop_until_quit`,
                    // and main takes the tunnel down on its way out.
                    MENU_QUIT => {
                        remove_tray_icon(hwnd);
                        let _ = slint::quit_event_loop();
                    }
                    _ => {}
                }
                LRESULT(0)
            }

            WM_NCDESTROY => {
                remove_tray_icon(hwnd);
                let res = DefSubclassProc(hwnd, umsg, wparam, lparam);
                let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
                drop(Box::from_raw(refdata as *mut Ctx));
                res
            }

            _ => DefSubclassProc(hwnd, umsg, wparam, lparam),
        }
    }
}

// ── Permanent acrylic blur (SetWindowCompositionAttribute, undocumented) ──────
// Unlike DWMWA_SYSTEMBACKDROP_TYPE (which switches off when the window loses
// focus), ACCENT_ENABLE_ACRYLICBLURBEHIND stays on whatever happens.

#[repr(C)]
struct AccentPolicy {
    accent_state: i32,
    accent_flags: u32,
    gradient_color: u32, // AABBGGRR
    animation_id: u32,
}

#[repr(C)]
struct WinCompAttrData {
    attrib: u32,
    pv_data: *mut c_void,
    cb_data: usize,
}

const ACCENT_ENABLE_ACRYLICBLURBEHIND: i32 = 4;
const WCA_ACCENT_POLICY: u32 = 19;

// ── Tray icon ────────────────────────────────────────────────────────────────
//
// The client runs in the background: closing the window hides it and leaves the
// tunnel alone, and only "Close" in the tray menu really quits. Slint's own
// tray support is `ksni`, which is D-Bus, so Windows goes through the shell
// directly — no new dependency, and the callbacks land in the window procedure
// this file already owns.

/// The message Explorer broadcasts once it has recreated the notification area.
/// Registered once; the id is the same for every window that asks.
fn taskbar_created() -> u32 {
    static ID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *ID.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

/// Fills the fixed-size tooltip buffer the shell expects.
fn tip(text: &str, into: &mut [u16; 128]) {
    let mut wide: Vec<u16> = text.encode_utf16().take(into.len() - 1).collect();
    wide.push(0);
    into[..wide.len()].copy_from_slice(&wide);
}

unsafe fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    }
}

/// Registers the icon. Returns whether the shell took it.
///
/// It can refuse while Explorer is still coming up — at logon, or in the moment
/// after it restarts — so the caller retries rather than leaving the client with
/// no way back to its own window.
unsafe fn add_tray_icon(hwnd: HWND) -> bool {
    unsafe {
        let mut data = tray_data(hwnd);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        tip("ValiraVPN", &mut data.szTip);

        if let Ok(instance) = GetModuleHandleW(None) {
            if let Ok(icon) = LoadImageW(
                Some(instance.into()),
                PCWSTR(1 as *const u16),
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_DEFAULTCOLOR,
            ) {
                data.hIcon = HICON(icon.0);
            }
        }
        Shell_NotifyIconW(NIM_ADD, &data).as_bool()
    }
}

/// Keeps trying for a few seconds, on the interface thread, without blocking it.
fn ensure_tray_icon(ui: &crate::App, attempts: u32) {
    let Some(hwnd) = window_hwnd(ui) else {
        return;
    };
    if unsafe { add_tray_icon(hwnd) } || attempts == 0 {
        return;
    }
    let weak = ui.as_weak();
    slint::Timer::single_shot(core::time::Duration::from_millis(400), move || {
        if let Some(ui) = weak.upgrade() {
            ensure_tray_icon(&ui, attempts - 1);
        }
    });
}

/// What the icon says on hover. A background client is often the only thing
/// telling you whether the tunnel is up.
pub fn set_tray_tooltip(ui: &crate::App, text: &str) {
    let Some(hwnd) = window_hwnd(ui) else {
        return;
    };
    unsafe {
        let mut data = tray_data(hwnd);
        data.uFlags = NIF_TIP;
        tip(text, &mut data.szTip);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    unsafe {
        let data = tray_data(hwnd);
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

unsafe fn reveal(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

unsafe fn show_tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open ValiraVPN"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Close"));

        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without this the menu refuses to dismiss when clicked away from.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            at.x,
            at.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
    }
}

/// Gives the window the application icon.
///
/// The icon compiled into the executable is what Explorer shows for the *file*
/// and what a pinned shortcut inherits. A running window's taskbar button and
/// its Alt-Tab entry come from the icon set on the *window*, and winit sets
/// none — so without this the taskbar falls back to the generic default while
/// the executable's own icon looks perfectly correct in Explorer.
///
/// Loaded from our own resource, id 1, the one `windows/valira.rc` declares.
/// Both sizes are requested explicitly so Windows picks the 16-pixel image for
/// the small slot instead of shrinking a large one.
unsafe fn apply_window_icon(hwnd: HWND) {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return;
        };
        // MAKEINTRESOURCE(1): the numeric id, not a name.
        let id = PCWSTR(1 as *const u16);

        let set = |metric, slot| {
            let size = GetSystemMetrics(metric);
            if let Ok(icon) = LoadImageW(
                Some(instance.into()),
                id,
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            ) {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(slot)), Some(LPARAM(icon.0 as isize)));
            }
        };
        set(SM_CXSMICON, ICON_SMALL as usize);
        set(SM_CXICON, ICON_BIG as usize);
    }
}

unsafe fn apply_backdrop(hwnd: HWND, acrylic: bool) {
    unsafe {
        if !acrylic {
            return;
        }
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        enable_acrylic(hwnd);
    }
}

unsafe fn refresh_backdrop_size(hwnd: HWND, ctx: &Ctx) {
    unsafe {
        if !ctx.acrylic {
            return;
        }
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_err() {
            return;
        }
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        if w == ctx.last_client_w.get() && h == ctx.last_client_h.get() {
            return;
        }
        ctx.last_client_w.set(w);
        ctx.last_client_h.set(h);
        apply_backdrop(hwnd, ctx.acrylic);
        let _ = RedrawWindow(
            Some(hwnd),
            None,
            None,
            RDW_INVALIDATE | RDW_FRAME | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
    }
}

unsafe fn enable_acrylic(hwnd: HWND) {
    unsafe {
        let Ok(hmod) = GetModuleHandleW(w!("user32.dll")) else {
            return;
        };
        let Some(proc) = GetProcAddress(hmod, s!("SetWindowCompositionAttribute")) else {
            return;
        };
        type SwcaFn = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> i32;
        let swca: SwcaFn = core::mem::transmute(proc);

        // gradient_color: black at roughly 25% alpha (most of the black comes
        // from the 70% panels themselves).
        let mut accent = AccentPolicy {
            accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
            accent_flags: 0,
            gradient_color: 0x4000_0000,
            animation_id: 0,
        };
        let mut data = WinCompAttrData {
            attrib: WCA_ACCENT_POLICY,
            pv_data: (&mut accent as *mut AccentPolicy).cast(),
            cb_data: core::mem::size_of::<AccentPolicy>(),
        };
        let _ = swca(hwnd, &mut data);
    }
}

/// Asks an already-running client to shut down, and waits for it to go.
///
/// The window is found by its title. Nothing else about it is needed: the
/// message is only a request, and the client answers it on its own event loop,
/// so the tunnel comes down through the ordinary exit path rather than being
/// cut off by a terminated process.
///
/// Returns false when no client was running, which is not a failure — that is
/// the state the caller wanted.
pub fn ask_running_client_to_quit(patience: std::time::Duration) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

    let title = HSTRING::from("ValiraVPN");
    let Ok(hwnd) = (unsafe { FindWindowW(None, &title) }) else {
        return false;
    };
    if hwnd.0.is_null() {
        return false;
    }

    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_QUIT_REQUEST, WPARAM(0), LPARAM(0));
    }

    // Gone once the window is: the process tears the tunnel down after its
    // event loop returns, so this waits for the window rather than the exit.
    let deadline = std::time::Instant::now() + patience;
    while std::time::Instant::now() < deadline {
        let still_there = unsafe { FindWindowW(None, &title) }
            .map(|h| !h.0.is_null())
            .unwrap_or(false);
        if !still_there {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    true
}

/// The name that stands for "a client is running on this machine".
///
/// Kept out of the `Global\` namespace on purpose: the client always runs
/// elevated, so every instance shares one session, and a machine-wide name
/// would additionally collide across concurrent user sessions on a shared
/// machine — where two people are entitled to their own client.
const SOLE_INSTANCE: &str = "ValiraVPN.SingleInstance";

/// The handle, kept alive for as long as this process is the client. Held as a
/// raw value because a `HANDLE` is not `Sync`; nothing is done with it except
/// closing it.
static INSTANCE_HELD: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Whether this process is the client, or a second copy that should stand down.
///
/// Taking the name is what makes it the client. A second launch finds the name
/// taken and gets `false`; it has no business opening a window, since the tunnel
/// belongs to whichever process holds the interface, and two clients pointing at
/// one tunnel disagree about its state the moment either touches it.
pub fn claim_sole_instance() -> bool {
    use std::sync::atomic::Ordering;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    unsafe {
        let name = HSTRING::from(SOLE_INSTANCE);
        // Not asking for ownership: existence is the whole question, and an
        // owned mutex left behind by a crash reports itself as abandoned, which
        // would only be one more state to reason about.
        let Ok(handle) = CreateMutexW(None, false, &name) else {
            // The name could not be taken at all. Refusing to start over that
            // would be worse than the duplicate it is meant to prevent.
            return true;
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return false;
        }
        INSTANCE_HELD.store(handle.0 as isize, Ordering::SeqCst);
        true
    }
}

/// Gives the name back, so a process this one is about to start can take it.
///
/// Used by the renderer fallback, which relaunches this program with the
/// software renderer pinned: the child would otherwise find the name held by
/// the parent that spawned it and stand down, leaving no client at all on
/// exactly the machines that need the fallback.
pub fn release_sole_instance() {
    use std::sync::atomic::Ordering;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};

    let raw = INSTANCE_HELD.swap(0, Ordering::SeqCst);
    if raw != 0 {
        unsafe {
            let _ = CloseHandle(HANDLE(raw as *mut core::ffi::c_void));
        }
    }
}

/// Brings the running client's window up.
///
/// What a refused second launch does before it goes. Someone who double-clicks
/// the icon while the window is hidden in the tray means "show me the client",
/// and answering that with nothing at all looks like a program that failed to
/// start.
pub fn reveal_running_client() {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

    let title = HSTRING::from("ValiraVPN");
    let Ok(hwnd) = (unsafe { FindWindowW(None, &title) }) else {
        return;
    };
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_COMMAND,
            WPARAM(MENU_OPEN),
            LPARAM(0),
        );
    }
}
