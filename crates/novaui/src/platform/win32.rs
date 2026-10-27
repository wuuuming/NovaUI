//! Win32 host: one window with a custom title bar (the client area covers the whole window; DWM
//! keeps the shadow and snap), per-monitor DPI v2, a message pump that blocks when there is no
//! work, frame pacing on the swap chain's latency object, input translation, IME placement,
//! drag-and-drop, file dialogs and a cross-thread waker.

use super::{App, Options};
use crate::app::{NcHit, Ui};
use crate::input::{Input, Mods, MouseButton};
use crate::render::primitive::Backend;
use crate::runner::{self, Pacer};
use crate::style::Cursor;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::Ime::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[cfg(test)]
#[path = "win32_capture_tests.rs"]
mod capture_tests;
#[path = "win32_vblank.rs"]
pub(super) mod vblank;
#[path = "window_state.rs"]
mod window_state;
use window_state::{SizeUpdate, WindowState};

pub const WM_APP_WAKE: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_NCMOUSELEAVE: u32 = 0x02A2;

/// Wakes the host's loop from any thread. The app's own wakers (`Ui::waker`) call it.
#[derive(Clone)]
pub(crate) struct HostWake {
    hwnd: Arc<AtomicIsize>,
    pending: Arc<AtomicBool>,
}

impl HostWake {
    pub fn wake(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            let h = self.hwnd.load(Ordering::Acquire);
            if h != 0 {
                // SAFETY: a handle and plain values; PostMessageW is callable from any thread
                // and fails (ignored) on a window that is gone.
                unsafe {
                    let _ = PostMessageW(HWND(h as _), WM_APP_WAKE, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

thread_local! {
    static QUEUE: RefCell<Vec<Msg>> = const { RefCell::new(Vec::new()) };
    /// The running app. The main loop and the window procedure both step it: during Win32's
    /// modal size/move loop only the window procedure runs, so it renders from WM_SIZE itself.
    static RUNNER: RefCell<Option<Runner>> = const { RefCell::new(None) };
    static MIN_SIZE: RefCell<(i32, i32)> = const { RefCell::new((320, 200)) };
    static RELEASING_CAPTURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

enum Msg {
    In(Input),
    Size(i32, i32, u32),
    Dpi,
    DisplayChanged,
    Paint,
    /// The modal size/move loop ended.
    SizeMoveEnd,
    Wake,
    Quit,
}

fn push(m: Msg) {
    QUEUE.with(|q| q.borrow_mut().push(m));
}

fn queued() -> bool {
    QUEUE.with(|q| !q.borrow().is_empty())
}

/// Steps the app now, unless a step is already running (a message sent from inside it): then the
/// message stays queued for the running step's loop.
fn step_now() {
    RUNNER.with(|r| {
        if let Ok(mut g) = r.try_borrow_mut() {
            if let Some(r) = g.as_mut() {
                r.step();
            }
        }
    });
}

/// The window's system menu at screen `(x, y)`, its items enabled for the window's state, and
/// the chosen command carried out as Windows' own menu would.
fn show_system_menu(hwnd: HWND, x: i32, y: i32) {
    // SAFETY: handles and plain values on the window's thread; the system menu is the window's
    // own (not destroyed by us), and TrackPopupMenu takes no rectangle (None).
    unsafe {
        let menu = GetSystemMenu(hwnd, false);
        if menu.is_invalid() {
            return;
        }
        let (max, min) = (is_zoomed(hwnd), is_iconic(hwnd));
        let on = |b: bool| if b { MF_ENABLED } else { MF_GRAYED };
        for (item, enabled) in [
            (SC_RESTORE, max || min),
            (SC_MOVE, !max),
            (SC_SIZE, !max),
            (SC_MINIMIZE, !min),
            (SC_MAXIMIZE, !max),
            (SC_CLOSE, true),
        ] {
            let _ = EnableMenuItem(menu, item, MF_BYCOMMAND | on(enabled));
        }
        let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN;
        let cmd = TrackPopupMenu(menu, flags, x, y, 0, hwnd, None);
        if cmd.0 != 0 {
            let _ = PostMessageW(hwnd, WM_SYSCOMMAND, WPARAM(cmd.0 as usize), LPARAM(0));
        }
    }
}

thread_local! {
    /// The refresh thread while the modal size/move loop runs (`vblank`).
    static VBLANK: RefCell<Option<vblank::Vblank>> = const { RefCell::new(None) };
}

// The window's own state, per thread: each window runs on a thread of its own (an overlay
// beside the main window), and its procedure reads only its own.
thread_local! {
    /// Inside Win32's modal size/move loop.
    static IN_SIZEMOVE: Cell<bool> = const { Cell::new(false) };
    /// The window is fullscreen (borderless over its monitor): no frame inset, no resize border.
    static FULLSCREEN: Cell<bool> = const { Cell::new(false) };
    /// The pointer's leaving is tracked (`TrackMouseEvent`).
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn mods() -> Mods {
    // SAFETY: plain values; reads this thread's keyboard state.
    unsafe {
        Mods {
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

/// DWM accepted the Windows 11 corner and border attributes.
static WIN11: AtomicBool = AtomicBool::new(false);

// ---- Win32 calls that need nothing but a window handle ---------------------------------------
//
// windows-rs marks every Win32 function `unsafe`. These take a handle and plain values: Win32
// checks the handle itself (a stale one fails the call; it is not undefined behaviour) and keeps
// no pointer past the call, so they are safe to call from the window's thread.

fn is_zoomed(hwnd: HWND) -> bool {
    // SAFETY: a handle and no pointers (see above).
    unsafe { IsZoomed(hwnd) }.as_bool()
}

fn is_iconic(hwnd: HWND) -> bool {
    // SAFETY: a handle and no pointers (see above).
    unsafe { IsIconic(hwnd) }.as_bool()
}

/// The window's DPI, at least 96 (a failed call returns 0).
fn dpi_of(hwnd: HWND) -> u32 {
    // SAFETY: a handle and no pointers (see above).
    unsafe { GetDpiForWindow(hwnd) }.max(96)
}

/// The resize border's thickness at the window's DPI: sizing frame plus padded border.
fn frame_thickness(hwnd: HWND) -> i32 {
    let dpi = dpi_of(hwnd);
    // SAFETY: plain values (see above).
    unsafe { GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) }
}

/// The screen point packed in a mouse message's `lParam`, in the window's client coordinates.
fn to_client(hwnd: HWND, lp: LPARAM) -> POINT {
    let (x, y) = xy(lp);
    let mut pt = POINT { x, y };
    // SAFETY: `pt` is a live local for the call; a stale handle only fails it (see above).
    let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
    pt
}

/// Windows' default handling of a message.
fn default_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: the message is passed on exactly as Windows delivered it to `wndproc`, so any
    // pointer in `wp` / `lp` is still the one Windows made valid for this call.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// The window procedure. Windows calls it on the window's thread with a live `hwnd`; a pointer
/// carried in `lp` points at the structure its message documents, valid until it returns.
extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wp.0 != 0 => {
            // The client area is the whole window. When maximised, Windows positions the
            // window beyond the monitor by the frame thickness: inset by it.
            // Fullscreen covers the monitor exactly (rcMonitor): nothing lies off it.
            if is_zoomed(hwnd) && !FULLSCREEN.with(Cell::get) {
                let f = frame_thickness(hwnd);
                // SAFETY: with wParam TRUE, `lp` points at Windows' NCCALCSIZE_PARAMS for this
                // call, ours to change until we return; no other reference to it exists.
                let p = unsafe { &mut *(lp.0 as *mut NCCALCSIZE_PARAMS) };
                p.rgrc[0].left += f;
                p.rgrc[0].right -= f;
                p.rgrc[0].top += f;
                p.rgrc[0].bottom -= f;
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let pt = to_client(hwnd, lp);
            let mut rc = RECT::default();
            // SAFETY: `rc` is a live local for the call.
            let _ = unsafe { GetClientRect(hwnd, &mut rc) };
            let hit = RUNNER.with(|r| {
                r.try_borrow()
                    .ok()
                    .and_then(|g| g.as_ref().map(|r| r.app.nc_hit(&r.ui, pt.x, pt.y)))
                    .unwrap_or(NcHit::Client)
            });
            if hit == NcHit::ClientControl {
                return LRESULT(HTCLIENT as isize);
            }
            let b = frame_thickness(hwnd);
            if !is_zoomed(hwnd) && !FULLSCREEN.with(Cell::get) {
                use super::FrameBorder as B;
                if let Some(part) = B::at(pt.x, pt.y, rc.right, rc.bottom, b) {
                    let code = match part {
                        B::TopLeft => HTTOPLEFT,
                        B::Top => HTTOP,
                        B::TopRight => HTTOPRIGHT,
                        B::Right => HTRIGHT,
                        B::BottomRight => HTBOTTOMRIGHT,
                        B::Bottom => HTBOTTOM,
                        B::BottomLeft => HTBOTTOMLEFT,
                        B::Left => HTLEFT,
                    };
                    return LRESULT(code as isize);
                }
            }
            return LRESULT(match hit {
                NcHit::Client | NcHit::ClientControl => HTCLIENT,
                NcHit::Caption => HTCAPTION,
                // Caption buttons are the app's (drawn and clicked in the client); HTMAXBUTTON
                // makes Windows 11 show the snap layouts flyout on hover.
                NcHit::Maximize => HTMAXBUTTON,
                NcHit::Minimize | NcHit::Close => HTCLIENT,
            } as isize);
        }
        WM_NCMOUSEMOVE if wp.0 == HTMAXBUTTON as usize => {
            let pt = to_client(hwnd, lp);
            push(Msg::In(Input::Move(pt.x, pt.y)));
            return LRESULT(0);
        }
        WM_NCLBUTTONDOWN if wp.0 == HTMAXBUTTON as usize => {
            let pt = to_client(hwnd, lp);
            push(Msg::In(Input::Down(pt.x, pt.y, MouseButton::Left)));
            return LRESULT(0);
        }
        WM_NCLBUTTONUP if wp.0 == HTMAXBUTTON as usize => {
            let pt = to_client(hwnd, lp);
            push(Msg::In(Input::Up(pt.x, pt.y, MouseButton::Left)));
            return LRESULT(0);
        }
        WM_NCMOUSELEAVE => push(Msg::In(Input::Leave)),
        // A press on the caption is the app's first, an ordinary press (it closes a flyout,
        // drops focus), and is handled before Windows' move loop, which is modal and returns
        // on the release: a drag (snap included) or a plain click. The release is the app's
        // too, where the press went down. The second press of a double click comes as
        // WM_NCLBUTTONDBLCLK: the app's press and release, then Windows' maximize toggle.
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wp.0 == HTCAPTION as usize => {
            let pt = to_client(hwnd, lp);
            push(Msg::In(Input::Down(pt.x, pt.y, MouseButton::Left)));
            step_now();
            let r = default_proc(hwnd, msg, wp, lp);
            push(Msg::In(Input::Up(pt.x, pt.y, MouseButton::Left)));
            step_now();
            return r;
        }
        // A right press on the caption is the app's too; its release opens the system menu
        // (modal, so what the press closed is gone before it opens). Ours to open: with the
        // frame drawn by the app, DefWindowProc opens none, as Chromium also finds.
        WM_NCRBUTTONDOWN | WM_NCRBUTTONUP if wp.0 == HTCAPTION as usize => {
            let pt = to_client(hwnd, lp);
            let down = msg == WM_NCRBUTTONDOWN;
            push(Msg::In(if down {
                Input::Down(pt.x, pt.y, MouseButton::Right)
            } else {
                Input::Up(pt.x, pt.y, MouseButton::Right)
            }));
            step_now();
            if !down {
                let (sx, sy) = xy(lp);
                show_system_menu(hwnd, sx, sy);
            }
            return LRESULT(0);
        }
        WM_GETMINMAXINFO => {
            let dpi = dpi_of(hwnd) as i32;
            let (w, h) = MIN_SIZE.with(|m| *m.borrow());
            // SAFETY: `lp` points at Windows' MINMAXINFO for this call, ours to change until we
            // return; no other reference to it exists.
            let mi = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
            mi.ptMinTrackSize = POINT { x: w * dpi / 96, y: h * dpi / 96 };
            return LRESULT(0);
        }
        WM_SIZE => {
            let (w, h) = xy(lp);
            push(Msg::Size(w & 0xffff, h & 0xffff, wp.0 as u32));
            // Present the new size before returning: while the user drags the border the
            // window grows with its content, not ahead of it.
            step_now();
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE => push(Msg::DisplayChanged),
        WM_DPICHANGED => {
            // SAFETY: `lp` points at the suggested window RECT Windows keeps for this call.
            let r = unsafe { *(lp.0 as *const RECT) };
            push(Msg::Dpi);
            // SAFETY: a handle and plain values.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            return LRESULT(0);
        }
        // Inside the modal size/move loop our own loop does not run: a turn per display
        // refresh keeps animations, playback and wakes going (`vblank`; a window timer would
        // tick on the system's ~15.6 ms clock instead).
        WM_ENTERSIZEMOVE => {
            IN_SIZEMOVE.with(|v| v.set(true));
            let period = RUNNER.with(|r| r.try_borrow().ok()?.as_ref().map(|r| r.pacer.period)).unwrap_or(1.0 / 60.0);
            let thread = vblank::Vblank::start(hwnd, monitor_of(hwnd), period);
            VBLANK.with(|v| *v.borrow_mut() = Some(thread));
        }
        WM_EXITSIZEMOVE => {
            IN_SIZEMOVE.with(|v| v.set(false));
            push(Msg::SizeMoveEnd);
            VBLANK.with(|v| v.borrow_mut().take());
        }
        vblank::WM_APP_VBLANK => {
            VBLANK.with(|v| v.borrow().as_ref().map(vblank::Vblank::handled));
            if IN_SIZEMOVE.with(Cell::get) {
                step_now();
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            if !TRACKING.with(|v| v.replace(true)) {
                let mut t = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: `t` is a live local with its size set; Windows copies it.
                let _ = unsafe { TrackMouseEvent(&mut t) };
            }
            let (x, y) = xy(lp);
            push(Msg::In(Input::Move(x, y)));
        }
        // Another window took the mouse (Alt+Tab, a system menu): a drag in progress is off.
        WM_CAPTURECHANGED if lp.0 != hwnd.0 as isize => {
            // ReleaseCapture itself sends this synchronously. Mouse capture is not keyboard
            // focus, and our own release must not cancel the Up that commits a drag.
            if !RELEASING_CAPTURE.with(|v| v.get()) {
                push(Msg::In(Input::CaptureLost));
            }
        }
        WM_MOUSELEAVE => {
            TRACKING.with(|v| v.set(false));
            push(Msg::In(Input::Leave));
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (x, y) = xy(lp);
            // SAFETY: a handle, on the window's thread.
            unsafe { SetCapture(hwnd) };
            push(Msg::In(Input::Down(
                x,
                y,
                match msg {
                    WM_LBUTTONDOWN => MouseButton::Left,
                    WM_RBUTTONDOWN => MouseButton::Right,
                    _ => MouseButton::Middle,
                },
            )));
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = xy(lp);
            // SAFETY: a handle, on the window's thread.
            unsafe { SetCapture(hwnd) };
            push(Msg::In(Input::DoubleClick(x, y)));
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = xy(lp);
            // The capture stays while another button is still down (a right press during a
            // left drag must not drop the drag's pointer outside the window).
            let held = (wp.0 & (0x0001 | 0x0002 | 0x0010)) != 0; // MK_LBUTTON | MK_RBUTTON | MK_MBUTTON
            push(Msg::In(Input::Up(
                x,
                y,
                match msg {
                    WM_LBUTTONUP => MouseButton::Left,
                    WM_RBUTTONUP => MouseButton::Right,
                    _ => MouseButton::Middle,
                },
            )));
            // SAFETY: no arguments; reads this thread's capture.
            if !held && unsafe { GetCapture() } == hwnd {
                RELEASING_CAPTURE.with(|v| {
                    let previous = v.replace(true);
                    // SAFETY: no arguments; releases this thread's capture.
                    let _ = unsafe { ReleaseCapture() };
                    v.set(previous);
                });
            }
        }
        WM_XBUTTONUP => {
            // Mouse "back" button → Browser Back key.
            if (wp.0 >> 16) & 0xffff == 1 {
                push(Msg::In(Input::Key(crate::input::key::BROWSER_BACK, Mods::default())));
            }
            return LRESULT(1);
        }
        WM_MOUSEWHEEL => {
            let p = to_client(hwnd, lp);
            let d = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
            push(Msg::In(Input::Wheel(p.x, p.y, d)));
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u32;
            // Alt+F4 and system keys keep their default handling.
            if !(msg == WM_SYSKEYDOWN && vk == VK_F4.0 as u32) {
                push(Msg::In(Input::Key(vk, mods())));
                if msg == WM_SYSKEYDOWN && vk != VK_MENU.0 as u32 {
                    return LRESULT(0);
                }
            }
        }
        // Inline composition (WinUI TextBox behaviour): the IME's own composition window is
        // turned off; its string is drawn in the field, its candidates placed under it.
        WM_IME_SETCONTEXT => {
            // Only flag bits change: `lp` of WM_IME_SETCONTEXT carries no pointer.
            let lp2 = LPARAM(lp.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
            return default_proc(hwnd, msg, wp, lp2);
        }
        WM_IME_STARTCOMPOSITION => return LRESULT(0),
        WM_IME_COMPOSITION => {
            ime_composition(hwnd, lp.0 as u32);
            step_now();
            return LRESULT(0);
        }
        WM_IME_ENDCOMPOSITION => {
            push(Msg::In(Input::Preedit(String::new(), 0)));
            step_now();
            return LRESULT(0);
        }
        WM_CHAR => {
            let c = wp.0 as u32;
            if let Some(ch) = char::from_u32(c) {
                push(Msg::In(Input::Char(ch)));
            } else if (0xD800..0xDC00).contains(&c) {
                HIGH.with(|h| *h.borrow_mut() = c as u16);
            } else if (0xDC00..0xE000).contains(&c) {
                let hi = HIGH.with(|h| *h.borrow());
                if let Some(Ok(ch)) = char::decode_utf16([hi, c as u16]).next() {
                    push(Msg::In(Input::Char(ch)));
                }
            }
            return LRESULT(0);
        }
        WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
            let c = CURSOR.with(|c| *c.borrow());
            let id = match c {
                Cursor::Arrow => IDC_ARROW,
                Cursor::Hand => IDC_HAND,
                Cursor::Text => IDC_IBEAM,
                Cursor::Grab | Cursor::Grabbing => IDC_SIZEALL,
                Cursor::ScrollNS => IDC_SIZENS,
            };
            // SAFETY: a system cursor id (no module, no pointer); SetCursor takes a handle.
            unsafe { SetCursor(LoadCursorW(None, id).unwrap_or_default()) };
            return LRESULT(1);
        }
        WM_DROPFILES => {
            push(Msg::In(Input::Files(dropped_files(HDROP(wp.0 as _)))));
            return LRESULT(0);
        }
        WM_SETFOCUS => push(Msg::In(Input::Focus(true))),
        WM_KILLFOCUS => push(Msg::In(Input::Focus(false))),
        WM_GETOBJECT => {
            if let Some(r) = access_getobject(wp, lp) {
                return r;
            }
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: `ps` is a live local, filled by BeginPaint and handed back to EndPaint.
            unsafe {
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            }
            push(Msg::Paint);
            step_now();
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_APP_WAKE => push(Msg::Wake),
        WM_CLOSE => {
            push(Msg::Quit);
            return LRESULT(0);
        }
        // SAFETY: a plain value; posts WM_QUIT to this thread's queue.
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        _ => {}
    }
    default_proc(hwnd, msg, wp, lp)
}

/// WM_IME_COMPOSITION: the committed string and the composition (with its caret) as inputs.
/// `flags` is the message's `lParam`: which strings changed.
fn ime_composition(hwnd: HWND, flags: u32) {
    // SAFETY: a handle; the context is released below.
    let imc = unsafe { ImmGetContext(hwnd) };
    if imc.is_invalid() {
        return;
    }
    let get = |kind: IME_COMPOSITION_STRING| -> String {
        // SAFETY: `imc` is the context taken above; a null buffer asks for the size in bytes.
        let n = unsafe { ImmGetCompositionStringW(imc, kind, None, 0) };
        if n <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; n as usize / 2];
        // SAFETY: `buf` holds `n` bytes, the size just reported; the IME writes no more.
        unsafe { ImmGetCompositionStringW(imc, kind, Some(buf.as_mut_ptr() as _), n as u32) };
        String::from_utf16_lossy(&buf)
    };
    if flags & GCS_RESULTSTR.0 != 0 {
        let r = get(GCS_RESULTSTR);
        if !r.is_empty() {
            push(Msg::In(Input::Commit(r)));
        }
    }
    if flags & GCS_COMPSTR.0 != 0 {
        let c = get(GCS_COMPSTR);
        // SAFETY: `imc` is the context taken above; GCS_CURSORPOS returns a value, no buffer.
        let cur = unsafe { ImmGetCompositionStringW(imc, GCS_CURSORPOS, None, 0) }.max(0) as usize;
        // GCS_CURSORPOS counts UTF-16 units: back to chars.
        let units: Vec<u16> = c.encode_utf16().collect();
        let cur = String::from_utf16_lossy(&units[..cur.min(units.len())]).chars().count();
        push(Msg::In(Input::Preedit(c, cur)));
    } else if flags & GCS_RESULTSTR.0 != 0 {
        push(Msg::In(Input::Preedit(String::new(), 0)));
    }
    // SAFETY: the context taken above, released once.
    let _ = unsafe { ImmReleaseContext(hwnd, imc) };
}

/// The paths of a WM_DROPFILES drop; the drop's memory is freed.
fn dropped_files(h: HDROP) -> Vec<PathBuf> {
    // SAFETY: `h` is the HDROP Windows passed with WM_DROPFILES, live until DragFinish below;
    // each buffer is sized from DragQueryFileW's own count plus the NUL it writes.
    unsafe {
        let n = DragQueryFileW(h, 0xFFFF_FFFF, None);
        let mut files = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(h, i, None);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(h, i, Some(&mut buf));
            files.push(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])));
        }
        DragFinish(h);
        files
    }
}

thread_local! {
    static HIGH: RefCell<u16> = const { RefCell::new(0) };
    static CURSOR: RefCell<Cursor> = const { RefCell::new(Cursor::Arrow) };
}

pub struct Host {
    pub hwnd: HWND,
    be: Box<dyn Backend>,
    backend: super::BackendKind,
    /// The window was made for DirectComposition (`composed`): a rebuilt backend composes too.
    composition: bool,
    t0: Instant,
    pending: Arc<AtomicBool>,
    placement: Option<WINDOWPLACEMENT>,
    refresh_monitor: HMONITOR,
    /// The app asked to close: the window is destroyed, no turn runs again.
    closing: bool,
    window_state: WindowState,
    /// This turn's events changed the window's size or scale: its frame follows the border.
    resizing: bool,
    /// The output the window is on (`win32_hdr`), HDR or SDR, as the backend was last told.
    outputs: super::win32_hdr::Outputs,
    hdr: Option<crate::render::primitive::HdrOutput>,
    /// The output is asked about again in the next turn (the first, a move, a display change).
    output_check: bool,
    /// The window's accessibility tree (UI Automation through `ACCESS`), when the app asked for it.
    access: Option<crate::access::Bridge>,
}

thread_local! {
    /// The window's UI Automation adapter and its activation handler: `wndproc` answers
    /// `WM_GETOBJECT` with them, the turn sends updates through them (`UiaAccess`).
    static ACCESS: RefCell<Option<(accesskit_windows::Adapter, crate::access::Handler)>> = const { RefCell::new(None) };
}

/// `WM_GETOBJECT`: UI Automation asks for the window's provider. The first ask only notes that
/// a client wants the tree (built in the next turn); the answer's call into UIA is made outside
/// the adapter's borrow, as it can send a nested `WM_GETOBJECT`.
fn access_getobject(wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let answer = ACCESS.with(|a| {
        let mut g = a.try_borrow_mut().ok()?;
        let (adapter, handler) = g.as_mut()?;
        adapter.handle_wm_getobject(accesskit_windows::WPARAM(wp.0), accesskit_windows::LPARAM(lp.0), handler)
    })?;
    let r: accesskit_windows::LRESULT = answer.into();
    Some(LRESULT(r.0))
}

/// The turn's side of the adapter in `ACCESS`. Its events are raised outside the borrow: a
/// client may call back into the window procedure.
struct UiaAccess;

impl crate::access::Adapter for UiaAccess {
    fn update(&mut self, make: &mut dyn FnMut() -> accesskit::TreeUpdate) {
        let events = ACCESS.with(|a| a.try_borrow_mut().ok()?.as_mut()?.0.update_if_active(make));
        if let Some(e) = events {
            e.raise();
        }
    }

    fn window_focus(&mut self, on: bool) {
        let events = ACCESS.with(|a| a.try_borrow_mut().ok()?.as_mut()?.0.update_window_focus_state(on));
        if let Some(e) = events {
            e.raise();
        }
    }
}

fn client(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    // SAFETY: `r` is a live local for the call; a stale handle only fails it.
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    (r.right - r.left, r.bottom - r.top)
}

/// A D3D11 backend presenting through DirectComposition, made before its window: the window's
/// style depends on it (`WS_EX_NOREDIRECTIONBITMAP`, fixed at creation). This is the fastest way
/// to the screen (a live drag reallocates nothing per step; `D3d11::compose`); `None` sends the
/// window to a swap chain of its own. Probed by trying, never by OS version, so RDP, VMs and
/// broken drivers fall back too. `NOVAUI_PRESENT=flip` forces the fallback,
/// `NOVAUI_PRESENT=composition` (the default) asks for this.
fn composed(kind: super::BackendKind) -> Option<crate::render::d3d11::D3d11> {
    match std::env::var("NOVAUI_PRESENT").ok().as_deref() {
        None | Some("") | Some("composition") => {}
        Some("flip") => return None,
        Some(other) => eprintln!("novaui: NOVAUI_PRESENT={other}: not a present mode (composition, flip)"),
    }
    if !matches!(kind, super::BackendKind::Default | super::BackendKind::D3d11) {
        return None;
    }
    let mut d = crate::render::d3d11::D3d11::new(1, 1).ok()?;
    match d.compose() {
        Ok(()) => Some(d),
        Err(e) => {
            eprintln!("novaui: no DirectComposition ({e}); presenting through the window");
            None
        }
    }
}

/// The window's backend: `composed` shown in it when there is one, else `kind`'s, falling back to
/// D3D11 and then software.
fn make_backend(
    kind: super::BackendKind,
    hwnd: HWND,
    w: i32,
    h: i32,
    composed: Option<crate::render::d3d11::D3d11>,
) -> Box<dyn Backend> {
    if let Some(mut d) = composed {
        match d.compose_into(hwnd, w, h) {
            Ok(()) => return Box::new(d),
            Err(e) => eprintln!("novaui: DirectComposition target: {e}; presenting through the window"),
        }
    }
    if kind == super::BackendKind::Vulkan {
        if let Ok(mut v) = crate::render::vulkan::Vulkan::new(w, h) {
            // SAFETY: None asks for this executable's module; no pointer is passed.
            let hi = unsafe { GetModuleHandleW(None).map(|m| m.0 as isize).unwrap_or(0) };
            if v.attach(crate::render::vulkan::Window::Win32 { hinstance: hi, hwnd: hwnd.0 as isize }, w, h).is_ok() {
                return Box::new(v);
            }
        }
    }
    if kind != super::BackendKind::Software {
        if let Ok(mut d) = crate::render::d3d11::D3d11::new(w, h) {
            if d.attach(hwnd, w, h).is_ok() {
                return Box::new(d);
            }
        }
    }
    let mut s = crate::render::software::Software::new(w, h);
    s.hwnd = Some(hwnd);
    Box::new(s)
}

impl Host {
    /// Borderless fullscreen over the window's monitor, after Raymond Chen ("How do I switch a
    /// window between normal and fullscreen?") and Chromium's `FullscreenHandler`: the caption and
    /// sizing frame off, the window over `rcMonitor`, the taskbar told (`MarkFullscreenWindow`).
    /// A maximised window first leaves its maximised state (else Windows keeps treating it as
    /// zoomed: the frame inset stays and the size is the work area's), and goes back to it after.
    /// Windows 11 rounds no corner of it (DWMWCP_DONOTROUND) while it covers the monitor.
    fn set_fullscreen(&mut self, on: bool) {
        // SAFETY: Win32 calls on the window's thread with its live handle; every structure passed
        // is a local with its size field set, every attribute pointer a local of the size given.
        unsafe {
            let hwnd = self.hwnd;
            if on && self.placement.is_none() {
                let mut p =
                    WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(hwnd, &mut p);
                self.placement = Some(p);
                let mon = monitor_of(hwnd);
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(mon, &mut mi);
                FULLSCREEN.with(|v| v.set(true));
                let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
                // WS_MAXIMIZE off with the frame: the window is now a plain popup-sized one.
                SetWindowLongW(hwnd, GWL_STYLE, (style.0 & !(WS_OVERLAPPEDWINDOW.0 | WS_MAXIMIZE.0)) as i32);
                set_corners(hwnd, 1); // DWMWCP_DONOTROUND
                let r = mi.rcMonitor;
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOP,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                );
                mark_fullscreen(hwnd, true);
            } else if !on {
                if let Some(p) = self.placement.take() {
                    // Returning from fullscreen is one viewport transition, not a fresh maximize
                    // from rcNormalPosition. Suppress that intermediate DWM zoom animation.
                    let disabled: i32 = 1;
                    let suppress_zoom = p.showCmd == SW_SHOWMAXIMIZED.0 as u32
                        && DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &disabled as *const _ as _, 4)
                            .is_ok();
                    FULLSCREEN.with(|v| v.set(false));
                    mark_fullscreen(hwnd, false);
                    let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
                    SetWindowLongW(hwnd, GWL_STYLE, (style.0 | WS_OVERLAPPEDWINDOW.0) as i32);
                    set_corners(hwnd, 2); // DWMWCP_ROUND
                                          // SetWindowPlacement restores the normal rect and, for a window that was
                                          // maximised, maximises it again (showCmd SW_SHOWMAXIMIZED), as Windows' own
                                          // maximise does: the zoomed state is Windows', not a copied size.
                    let _ = SetWindowPlacement(hwnd, &p);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                    );
                    if suppress_zoom {
                        // Commit the final placement before allowing ordinary window animations
                        // again; otherwise the compositor can observe only the re-enabled state.
                        let _ = DwmFlush();
                        let enabled: i32 = 0;
                        let _ =
                            DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &enabled as *const _ as _, 4);
                    }
                }
            }
        }
    }
}

/// Tells the shell the window is fullscreen (the taskbar goes under it while it is active) or
/// no longer is (the shell's own detection takes over).
fn mark_fullscreen(hwnd: HWND, on: bool) {
    // SAFETY: COM is initialised on this thread (`run`); the taskbar object is used and released
    // here; the window handle is a plain value to it.
    unsafe {
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        if let Ok(tb) = CoCreateInstance::<_, ITaskbarList2>(&TaskbarList, None, CLSCTX_INPROC_SERVER) {
            if tb.HrInit().is_ok() {
                let _ = tb.MarkFullscreenWindow(hwnd, on);
            }
        }
    }
}

/// Opens the window and runs `app` until the window closes. `Err` when no window could be had,
/// or no font loads.
pub fn run(opts: Options, mut make: impl FnMut(&mut Ui) -> Box<dyn App>) -> Result<(), crate::Error> {
    crate::text::check_fonts()?;
    let pre = composed(opts.backend);
    let composition = pre.is_some();
    let hwnd = open_window(&opts, composition).map_err(crate::Error::Platform)?;
    let (cw, ch) = client(hwnd);
    let scale = scale_of(hwnd);

    let be = make_backend(opts.backend, hwnd, cw, ch, pre);
    let pending = Arc::new(AtomicBool::new(false));
    let waker = HostWake { hwnd: Arc::new(AtomicIsize::new(hwnd.0 as isize)), pending: pending.clone() };
    let task_waker = waker.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || task_waker.wake());
    // UI Automation is set up before the window can be asked for it (never inside
    // `WM_GETOBJECT`, AccessKit's rule); no tree is built until a client asks.
    let access = opts.accessibility.then(|| {
        let shared = crate::access::Shared::new(wake.clone());
        let hwnd = accesskit_windows::HWND(hwnd.0);
        let adapter = accesskit_windows::Adapter::new(hwnd, false, crate::access::Handler(shared.clone()));
        ACCESS.with(|a| *a.borrow_mut() = Some((adapter, crate::access::Handler(shared.clone()))));
        crate::access::Bridge::new(shared, Box::new(UiaAccess), opts.title.clone())
    });
    let mut host = Host {
        hwnd,
        be,
        backend: opts.backend,
        composition,
        t0: Instant::now(),
        pending,
        placement: None,
        refresh_monitor: monitor_of(hwnd),
        closing: false,
        window_state: WindowState::default(),
        resizing: false,
        outputs: Default::default(),
        hdr: None,
        output_check: true,
        access,
    };
    let mut ui = Ui::new(cw, ch, scale);
    ui.window.handle = hwnd.0 as isize;
    ui.set_task_wake(wake);

    let mut app = make(&mut ui);
    // First frame before showing: no white flash.
    runner::start(&mut host, &mut ui, app.as_mut());

    // SAFETY: a handle and a plain value.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    RUNNER.with(|r| *r.borrow_mut() = Some(Runner { host, ui, app, pacer: Pacer::new(refresh_hz(hwnd)) }));

    let mut quit = false;
    while !quit {
        let timeout = RUNNER.with(|r| {
            let g = r.borrow();
            let r = g.as_ref().expect("the runner is installed before this loop and taken after it");
            // Pending paint is retained while minimized, but is not runnable work.
            let can_draw = runner::Host::visible(&r.host);
            r.pacer
                .timeout_ms(&r.ui, runner::Host::now(&r.host), can_draw, queued())
                .unwrap_or(windows::Win32::System::Threading::INFINITE)
        });
        if timeout > 0 {
            // SAFETY: no handles (None); waits on this thread's message queue.
            unsafe { MsgWaitForMultipleObjectsEx(None, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        }
        let mut msg = MSG::default();
        // SAFETY: `msg` is a live local, filled by PeekMessageW and read back by the dispatch;
        // dispatching runs `wndproc` on this thread.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    quit = true;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if quit {
            break;
        }
        step_now();
    }
    let r = RUNNER.with(|r| r.borrow_mut().take());
    if let Some(r) = r {
        drop(r);
    }
    Ok(())
}
/// A string as Win32 takes it: UTF-16, NUL-terminated.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// A dialog's file types as COMDLG_FILTERSPECs: `specs` point into `names`, wide strings this
/// value owns (their buffers stay put when it moves).
struct FilterSpecs {
    #[allow(dead_code)]
    names: Vec<(Vec<u16>, Vec<u16>)>,
    specs: Vec<windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC>,
}

impl FilterSpecs {
    fn of(filters: &[(String, String)]) -> Self {
        let names: Vec<(Vec<u16>, Vec<u16>)> = filters.iter().map(|(n, p)| (wide(n), wide(p))).collect();
        let specs = names
            .iter()
            .map(|(n, p)| windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC {
                pszName: PCWSTR(n.as_ptr()),
                pszSpec: PCWSTR(p.as_ptr()),
            })
            .collect();
        FilterSpecs { names, specs }
    }
}

/// The window's scale: its DPI over 96.
fn scale_of(hwnd: HWND) -> f32 {
    dpi_of(hwnd) as f32 / 96.0
}

/// A new viewport (a size or a DPI) is still queued.
fn viewport_queued() -> bool {
    QUEUE.with(|q| q.borrow().iter().any(|m| matches!(m, Msg::Size(..) | Msg::Dpi)))
}

/// The window's DWM corner preference (`DWMWA_WINDOW_CORNER_PREFERENCE`); `false` where the
/// system has none (before Windows 11).
fn set_corners(hwnd: HWND, preference: i32) -> bool {
    // SAFETY: our window; the value is a live local of the 4 bytes the attribute takes.
    unsafe { DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(33), &preference as *const _ as _, 4) }.is_ok()
}

/// The monitor the window is mostly on (the nearest when it is on none).
pub(super) fn monitor_of(hwnd: HWND) -> HMONITOR {
    // SAFETY: a handle and a plain value.
    unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
}

/// Registers the window class and creates the (hidden) window: per-monitor DPI, COM on this
/// thread, dark caption, Windows 11 corners and edge, DWM's shadow, file drops.
fn open_window(opts: &Options, composition: bool) -> Result<HWND, String> {
    // SAFETY: process and thread setup, then Win32 calls with the new window's handle on this
    // thread; every structure and attribute passed is a live local (the title's UTF-16 buffer
    // outlives CreateWindowExW, which copies it); `wndproc` is the class's procedure for the
    // life of the process.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let _ =
            windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);

        let hi: HINSTANCE = GetModuleHandleW(None).map_err(|e| format!("module handle: {e}"))?.into();
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hi,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: LoadIconW(hi, PCWSTR(1 as _)).unwrap_or_default(),
            lpszClassName: w!("NovaUI"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        MIN_SIZE.with(|m| *m.borrow_mut() = (opts.min_width, opts.min_height));
        let title = wide(&opts.title);
        // Size in dips at the primary monitor's DPI; WM_DPICHANGED corrects it on another monitor.
        let dpi = GetDpiForSystem().max(96);
        let (w, h) = (opts.width * dpi as i32 / 96, opts.height * dpi as i32 / 96);
        // A composited window has no redirection surface: DirectComposition draws all of it.
        let ex = if composition { WS_EX_NOREDIRECTIONBITMAP } else { WINDOW_EX_STYLE(0) };
        let hwnd = CreateWindowExW(
            ex,
            w!("NovaUI"),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            w,
            h,
            None,
            None,
            hi,
            None,
        )
        .map_err(|e| format!("window: {e}"))?;

        // Dark caption colours for the (invisible) native frame and the snap previews.
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        // Windows 11 (build 22000+): rounded corners and DWM's 1-px edge in WinUI's dark stroke
        // (both dropped by DWM itself while maximized or snapped full). Earlier systems refuse
        // the attributes; the edge is then drawn by the tree (`set_frame_stroke`).
        let rounded = set_corners(hwnd, 2); // DWMWCP_ROUND
        let stroke: u32 = 0x00_3A3A3A; // COLORREF 0x00BBGGRR
        let stroked = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(34), &stroke as *const _ as _, 4).is_ok();
        WIN11.store(rounded && stroked, Ordering::Relaxed);
        // A 1-px top margin keeps DWM's shadow on a borderless client-area window.
        let m = windows::Win32::UI::Controls::MARGINS {
            cxLeftWidth: 0,
            cxRightWidth: 0,
            cyTopHeight: 1,
            cyBottomHeight: 0,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        DragAcceptFiles(hwnd, true);
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
        Ok(hwnd)
    }
}

struct Runner {
    host: Host,
    ui: Ui,
    app: Box<dyn App>,
    /// When the next turn runs (the refresh deadline, `runner::Pacer`).
    pacer: Pacer,
}

impl Runner {
    /// One turn (`runner::turn`), unless the window is gone.
    fn step(&mut self) {
        if !self.host.closing {
            runner::turn(&mut self.host, &mut self.ui, self.app.as_mut(), &mut self.pacer);
        }
    }
}

impl runner::Host for Host {
    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    /// A size step is drawn at once, in step with the border; so is every frame inside the modal
    /// size/move loop (waiting there would hold the border), and a minimised window's.
    fn live(&self) -> bool {
        let sizing = viewport_queued();
        sizing || IN_SIZEMOVE.with(Cell::get) || is_iconic(self.hwnd)
    }

    fn events(&mut self, ui: &mut Ui, _app: &mut dyn App, pacer: &mut Pacer) {
        let hwnd = self.hwnd;
        let msgs = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
        let mut resized = None;
        let mut instant_layout = false;
        let mut display_changed = false;
        for m in msgs {
            match m {
                Msg::In(i) => {
                    // Activated: the SDR white may have changed meanwhile (Settings sends nothing).
                    self.output_check |= matches!(i, Input::Focus(true));
                    ui.input(i)
                }
                Msg::Size(w, h, kind) => match self.window_state.resize(kind, w, h) {
                    SizeUpdate::Ignore => {}
                    SizeUpdate::Minimized => resized = None,
                    SizeUpdate::Apply { width, height, maximized, instant } => {
                        resized = Some((width, height));
                        ui.window.maximized = maximized;
                        instant_layout |= instant;
                    }
                },
                Msg::Dpi => {
                    resized = Some(client(hwnd));
                    display_changed = true;
                }
                Msg::DisplayChanged => display_changed = true,
                Msg::Paint => ui.tree.expose(),
                Msg::SizeMoveEnd => ui.tree.resize_ended(runner::Host::now(self)),
                Msg::Wake => {
                    super::woken(&self.pending, ui);
                }
                Msg::Quit => {
                    ui.window.req.close = true;
                }
            }
        }
        self.resizing = false;
        if let Some((w, h)) = resized {
            if w > 0 && h > 0 && !self.window_state.minimized && !is_iconic(hwnd) {
                let scale = scale_of(hwnd);
                self.resizing = (w, h) != ui.tree.window_size() || scale != ui.tree.scale();
                if instant_layout {
                    ui.snap_layout();
                }
                if self.resizing {
                    ui.resize(w, h, scale);
                }
            }
        }
        // Enumerating display modes is configuration work, not per-turn frame work.
        // Monitor changes are cheap to detect; refresh only on those or display/DPI events.
        let monitor = monitor_of(hwnd);
        if display_changed || monitor != self.refresh_monitor {
            pacer.set_hz(refresh_hz(hwnd));
            self.refresh_monitor = monitor;
            self.output_check = true;
        }
        // HDR or SDR: the backend draws for the output the window is on now.
        if std::mem::take(&mut self.output_check) || self.outputs.stale() {
            let hdr = self.outputs.of(hwnd);
            if hdr != self.hdr {
                match hdr {
                    Some(o) => eprintln!("novaui: HDR output (SDR white {} nits, peak {} nits)", o.sdr_white, o.peak),
                    None if self.hdr.is_some() => eprintln!("novaui: SDR output"),
                    None => {}
                }
            }
            self.hdr = hdr;
            if self.be.set_output(self.hdr) {
                ui.tree.invalidate_all();
            }
        }
    }

    fn requests(&mut self, ui: &mut Ui, req: super::Requests) {
        let hwnd = self.hwnd;
        if req.minimize {
            // SAFETY: a handle and a plain value.
            let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
        }
        if req.toggle_maximize {
            let show = if is_zoomed(hwnd) { SW_RESTORE } else { SW_MAXIMIZE };
            // SAFETY: a handle and a plain value.
            let _ = unsafe { ShowWindow(hwnd, show) };
        }
        if let Some(f) = req.fullscreen {
            self.set_fullscreen(f);
            ui.window.fullscreen = f;
        }
        if let Some(t) = req.title {
            if let Some(a) = &mut self.access {
                a.title = t.clone();
            }
            let t = wide(&t);
            // SAFETY: `t` is NUL-terminated and outlives the call, which copies it.
            let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(t.as_ptr())) };
        }
    }

    fn access(&mut self) -> Option<&mut crate::access::Bridge> {
        self.access.as_mut()
    }

    fn close(&mut self) {
        self.closing = true;
        // SAFETY: the window's own handle on its thread; nothing uses it after this turn.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }

    fn cursor_and_ime(&mut self, ui: &Ui) {
        let hwnd = self.hwnd;
        CURSOR.with(|c| *c.borrow_mut() = ui.cursor);
        // The IME composes only into a text field: elsewhere its context is detached, so keys
        // reach the app's shortcuts (Space, arrows) instead of the IME.
        if IME_ON.with(|c| c.replace(ui.ime_on())) != ui.ime_on() {
            ime_enable(hwnd, ui.ime_on());
        }
        if let Some((x, y, h)) = ui.io.ime_at {
            // The candidate window stands under the caret and never covers its line.
            let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y }, rcArea: RECT::default() };
            let cand = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_EXCLUDE,
                ptCurrentPos: POINT { x, y: y + h },
                rcArea: RECT { left: x, top: y, right: x + 1, bottom: y + h },
            };
            // SAFETY: the window's input context, taken and released here; the forms are live
            // locals the IME copies.
            unsafe {
                let imc = ImmGetContext(hwnd);
                if !imc.is_invalid() {
                    let _ = ImmSetCompositionWindow(imc, &cf);
                    let _ = ImmSetCandidateWindow(imc, &cand);
                    let _ = ImmReleaseContext(hwnd, imc);
                }
            }
        }
    }

    fn clipboard_out(&mut self, text: String) {
        set_clipboard_text(self.hwnd, &text);
    }

    fn clipboard_in(&mut self) -> Option<String> {
        clipboard_text()
    }

    fn picks(&mut self, ui: &mut Ui) {
        serve_picks(self.hwnd, ui);
    }

    /// Windows 10 draws no edge on a frameless window; Windows 11 draws its own (DWM).
    fn edge(&self, ui: &Ui) -> Option<crate::geom::Color> {
        let edge = !WIN11.load(Ordering::Relaxed) && ui.window.framed();
        edge.then_some(super::FRAME_EDGE)
    }

    fn may_draw(&mut self, ui: &Ui, pacer: &mut Pacer) -> bool {
        // Inside the modal loop, frames that are not the border's own (the refresh thread's:
        // playback, animations) go only when the GPU has retired the last present. Unthrottled,
        // they queued faster than the GPU drained and the driver's queue grew by ~100 MB.
        // They are also held to one per refresh: a timer plus every WM_PAINT once drove ~1700
        // immediate presents a second during a drag. A turn more than half a refresh after the
        // last present is a new refresh (the refresh thread's turns come one period apart, a
        // frame's own drawing time after it; a whole period here would skip every other one).
        let modal_tick = !self.resizing && IN_SIZEMOVE.with(Cell::get);
        let next_refresh = pacer.last_frame_at + pacer.period;
        let early = modal_tick && ui.now < next_refresh - 0.5 * pacer.period;
        let gated = modal_tick && (early || !self.be.frame_ready());
        pacer.hold = gated.then_some(next_refresh);
        // ShowWindow/fullscreen requests can synchronously enqueue another size while
        // this runner is borrowed. Process that final viewport before drawing, not an old
        // layout into a window whose native bounds have already changed.
        let pending_viewport = viewport_queued();
        if is_iconic(self.hwnd) || gated || pending_viewport {
            return false;
        }
        // A resize frame is synchronous with the border: it does not wait for the previous
        // present to retire. Nor does any frame inside the modal loop: waiting there would hold
        // the border.
        self.be.set_immediate(self.resizing || IN_SIZEMOVE.with(Cell::get));
        true
    }

    fn backend(&mut self) -> &mut dyn Backend {
        self.be.as_mut()
    }

    fn renew_backend(&mut self) -> bool {
        let (w, h) = client(self.hwnd);
        // Its composition target holds the window, which takes one.
        runner::replace_backend(&mut self.be, || {
            let pre = if self.composition { composed(self.backend) } else { None };
            Some(make_backend(self.backend, self.hwnd, w, h, pre))
        });
        // The new backend starts SDR; the runner has everything drawn again after this.
        self.be.set_output(self.hdr);
        true
    }

    fn visible(&self) -> bool {
        !self.window_state.minimized
    }
}

fn refresh_hz(hwnd: HWND) -> f64 {
    use windows::Win32::Graphics::Gdi::*;
    // SAFETY: the structures are live locals with their size fields set; MONITORINFOEXW begins
    // with MONITORINFO, as GetMonitorInfoW expects when cbSize says EX; the device name it
    // writes is NUL-terminated and read before `mi` goes.
    unsafe {
        let mon = monitor_of(hwnd);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi as *mut _ as *mut MONITORINFO).as_bool()
            && EnumDisplaySettingsW(PCWSTR(mi.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool()
        {
            return super::refresh_or_60(Some(dm.dmDisplayFrequency as f64));
        }
    }
    60.0
}

/// `platform::open` on its thread: the shell's "open" verb.
pub(super) fn open_now(target: &std::ffi::OsStr) -> Result<(), String> {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::ShellExecuteW;
    let wide: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(target).chain([0]).collect();
    // SAFETY: COM is set up for this thread and torn down once; the target is a NUL-terminated
    // string alive for the call, the verb a static one; no window is passed.
    let r = unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let r = ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL);
        if com {
            CoUninitialize();
        }
        r
    };
    // ShellExecute's answer above 32 is success; at or below it, an error code.
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecute error {}", r.0 as isize))
    }
}

/// Opens the pickers the app asked for (`Ui::pick`), each modal to the window as the system's
/// pickers are, and queues their answers as the app's messages.
fn serve_picks(hwnd: HWND, ui: &mut Ui) {
    for (pick, done, _site) in std::mem::take(&mut ui.io.picks) {
        let paths = pick.answer(|d, m, f| open_dialog(hwnd, d, m, f), |n, f| save_dialog(hwnd, n, f));
        ui.events.push(crate::Event::Message(done(paths)));
    }
}

/// Modal file-open dialog (folders instead of files when `folders`; several when `multi`): the
/// chosen paths, none when cancelled.
fn open_dialog(owner: HWND, folders: bool, multi: bool, filters: &[(String, String)]) -> Vec<PathBuf> {
    use windows::Win32::System::Com::*;
    // SAFETY: COM is initialised on this (the UI) thread; the filter strings (`wide`) are
    // NUL-terminated and outlive `specs` and the dialog's use of them; every string the dialog
    // returns is read, then freed once with CoTaskMemFree.
    unsafe {
        let Ok(d): windows::core::Result<IFileOpenDialog> =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
        else {
            return Vec::new();
        };
        let mut o = d.GetOptions().unwrap_or_default();
        o |= FOS_FORCEFILESYSTEM;
        if folders {
            o |= FOS_PICKFOLDERS;
        }
        if multi {
            o |= FOS_ALLOWMULTISELECT;
        }
        let _ = d.SetOptions(o);
        let filter_specs = FilterSpecs::of(filters);
        let specs = &filter_specs.specs;
        if !specs.is_empty() && !folders {
            let _ = d.SetFileTypes(specs);
        }
        if d.Show(owner).is_err() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Ok(items) = d.GetResults() {
            let n = items.GetCount().unwrap_or(0);
            for i in 0..n {
                if let Ok(it) = items.GetItemAt(i) {
                    if let Ok(p) = it.GetDisplayName(SIGDN_FILESYSPATH) {
                        out.push(PathBuf::from(p.to_string().unwrap_or_default()));
                        CoTaskMemFree(Some(p.0 as _));
                    }
                }
            }
        }
        out
    }
}

/// Modal file-save dialog; a name typed without an extension takes the first filter's.
fn save_dialog(owner: HWND, name: &str, filters: &[(String, String)]) -> Option<PathBuf> {
    use windows::Win32::System::Com::*;
    // SAFETY: as `open_dialog`: COM on this thread, NUL-terminated strings that outlive the
    // dialog's use, the returned name freed once.
    unsafe {
        let d: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let filter_specs = FilterSpecs::of(filters);
        let specs = &filter_specs.specs;
        if !specs.is_empty() {
            let _ = d.SetFileTypes(specs);
        }
        // `*.m3u8;*.m3u` → `m3u8`.
        if let Some(ext) = crate::app::FilePick::save_extension(filters) {
            let e = wide(ext);
            let _ = d.SetDefaultExtension(PCWSTR(e.as_ptr()));
        }
        let n = wide(name);
        let _ = d.SetFileName(PCWSTR(n.as_ptr()));
        d.Show(owner).ok()?;
        let it = d.GetResult().ok()?;
        let p = it.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

thread_local! {
    static IME_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Attaches the window's default input context (a field has the focus) or detaches it.
fn ime_enable(hwnd: HWND, on: bool) {
    let flags = if on { IACE_DEFAULT } else { 0 };
    // SAFETY: a handle, a null context and flags; Windows keeps the window's default context.
    let _ = unsafe { ImmAssociateContextEx(hwnd, HIMC::default(), flags) };
}

pub fn set_clipboard_text(hwnd: HWND, s: &str) {
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;
    let w = wide(s);
    // SAFETY: the clipboard is opened and closed here; the block is allocated with room for
    // `w` (its NUL included) and written only while locked; once SetClipboardData takes it, the
    // clipboard owns it and we touch it no more.
    unsafe {
        if OpenClipboard(hwnd).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                // The clipboard owns the memory once SetClipboardData succeeds.
                let _ = SetClipboardData(13, HANDLE(h.0 as _));
            }
        }
        let _ = CloseClipboard();
    }
}

/// The user's display language as a BCP 47 tag (`zh-CN`, `en-US`); empty if unknown.
pub fn locale() -> String {
    use windows::Win32::Globalization::{GetUserDefaultUILanguage, LCIDToLocaleName};
    let mut buf = [0u16; 85];
    // SAFETY: `buf` is LOCALE_NAME_MAX_LENGTH (85) units, the most the call writes.
    let n = unsafe { LCIDToLocaleName(GetUserDefaultUILanguage() as u32, Some(&mut buf), 0) };
    String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1)])
}

fn clipboard_text() -> Option<String> {
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::*;
    // SAFETY: the clipboard is opened and closed here; its block is read only while locked and
    // no further than GlobalSize says it holds, then unlocked once.
    unsafe {
        OpenClipboard(None).ok()?;
        let r = (|| {
            let h = GetClipboardData(13).ok()?; // CF_UNICODETEXT
            let block = HGLOBAL(h.0 as _);
            let p = GlobalLock(block) as *const u16;
            if p.is_null() {
                return None;
            }
            // The block is the other application's: read no further than its size, whether or
            // not it ends the text with a NUL.
            let s = unicode_text(std::slice::from_raw_parts(p, GlobalSize(block) / 2));
            let _ = GlobalUnlock(block);
            Some(s)
        })();
        let _ = CloseClipboard();
        r
    }
}

/// The text of a `CF_UNICODETEXT` block: up to its first NUL, or all of it when its writer left
/// none.
fn unicode_text(units: &[u16]) -> String {
    let n = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..n])
}

#[cfg(test)]
mod clipboard_tests {
    /// Regression: the read scanned for a NUL with no bound, so a foreign block
    /// without one was read past its end.
    #[test]
    fn unicode_text_stops_at_the_nul_or_the_end_of_the_block() {
        let text: Vec<u16> = "\u{6B4C}\u{8BCD} lyric".encode_utf16().collect();
        assert_eq!(super::unicode_text(&text), "\u{6B4C}\u{8BCD} lyric");
        let mut ended = text.clone();
        ended.extend([0, 'x' as u16]);
        assert_eq!(super::unicode_text(&ended), "\u{6B4C}\u{8BCD} lyric");
        assert_eq!(super::unicode_text(&[]), "");
    }
}
