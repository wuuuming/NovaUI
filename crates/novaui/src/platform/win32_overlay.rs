//! Win32 overlays (`OverlayOptions`): a topmost tool window that is never activated and shows
//! no frame, shadow or taskbar entry, drawn by DirectComposition with premultiplied alpha; its
//! place is its bottom centre, kept as it grows; and while it lets the pointer through
//! (`WS_EX_LAYERED | WS_EX_TRANSPARENT`) a low-level mouse hook tells it when the pointer
//! comes and goes over it.

use super::{push, Msg};
use crate::input::Input;
use crate::platform::{OverlayOptions, Place};
use std::cell::{Cell, RefCell};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    /// This thread's window is an overlay: no resize border, no system menu, never activated.
    pub(super) static OVERLAY: Cell<bool> = const { Cell::new(false) };
    /// The hook that follows the pointer while the overlay lets it through.
    static HOOK: RefCell<Option<PointerHook>> = const { RefCell::new(None) };
}

/// The low-level mouse hook of a click-through overlay, and whether the pointer was over it.
struct PointerHook {
    hook: HHOOK,
    hwnd: HWND,
    inside: bool,
}

/// The window rectangle an overlay `w`×`h` px stands in when its bottom centre is `place`.
pub(super) fn rect_at(place: Place, w: i32, h: i32) -> RECT {
    let left = place.x - w / 2;
    RECT { left, top: place.y - h, right: left + w, bottom: place.y }
}

/// The place of an overlay standing in `r`: its bottom centre (`rect_at`'s inverse).
pub(super) fn place_of(r: RECT) -> Place {
    Place { x: r.left + (r.right - r.left) / 2, y: r.bottom }
}

/// The default place: centred at the bottom of `work`, `lift` of its height above it.
fn default_place(work: RECT, lift: f32) -> Place {
    let lift = (lift.clamp(0.0, 1.0) * (work.bottom - work.top) as f32).round() as i32;
    Place { x: work.left + (work.right - work.left) / 2, y: work.bottom - lift }
}

fn monitor_info(mon: HMONITOR) -> Option<MONITORINFO> {
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `mi` is a live local with its size set.
    unsafe { GetMonitorInfoW(mon, &mut mi) }.as_bool().then_some(mi)
}

/// The DPI of a monitor, at least 96.
fn monitor_dpi(mon: HMONITOR) -> u32 {
    let (mut x, mut y) = (96u32, 96u32);
    // SAFETY: a handle and two live locals.
    let _ = unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    x.max(96)
}

/// Where the overlay opens: the kept place when a display shows it, else the default on the
/// main display. Its monitor comes with it.
fn opening_place(opts: &OverlayOptions) -> (Place, HMONITOR) {
    if let Some(p) = opts.place {
        // SAFETY: plain values.
        let mon = unsafe { MonitorFromPoint(POINT { x: p.x, y: p.y - 1 }, MONITOR_DEFAULTTONULL) };
        if !mon.is_invalid() {
            return (p, mon);
        }
    }
    // SAFETY: plain values; the origin is on the main display.
    let main = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let work = monitor_info(main).map(|m| m.rcWork).unwrap_or(RECT { left: 0, top: 0, right: 1280, bottom: 720 });
    (default_place(work, opts.lift), main)
}

/// The work area of the monitor the window is on, in dips.
pub(super) fn work_area(hwnd: HWND) -> Option<(f32, f32)> {
    let mon = super::monitor_of(hwnd);
    let r = monitor_info(mon)?.rcWork;
    let k = super::scale_of(hwnd);
    Some(((r.right - r.left) as f32 / k, (r.bottom - r.top) as f32 / k))
}

/// Creates the (hidden) overlay window at its place: a topmost, never-activated tool popup,
/// layered (so it can let the pointer through), with no corners, border or shadow from DWM.
pub(super) fn open(opts: &OverlayOptions) -> Result<(HWND, Place), String> {
    // SAFETY: process and thread setup, then Win32 calls with the new window's handle on this
    // thread; every structure and attribute passed is a live local (the title's buffer outlives
    // CreateWindowExW, which copies it); the class's procedure lives as long as the process.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ =
            windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let hi: HINSTANCE = GetModuleHandleW(None).map_err(|e| format!("module handle: {e}"))?.into();
        let wc = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(super::wndproc),
            hInstance: hi,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("NovaUI.Overlay"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        OVERLAY.with(|o| o.set(true));
        super::MIN_SIZE.with(|m| *m.borrow_mut() = (0, 0));
        let (place, mon) = opening_place(opts);
        let dpi = monitor_dpi(mon) as f32 / 96.0;
        let (w, h) = ((opts.width * dpi).round().max(1.0) as i32, (opts.height * dpi).round().max(1.0) as i32);
        let r = rect_at(place, w, h);
        let title = super::wide(&opts.title);
        let ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED;
        let hwnd = CreateWindowExW(
            ex,
            w!("NovaUI.Overlay"),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            r.left,
            r.top,
            w,
            h,
            None,
            None,
            hi,
            None,
        )
        .map_err(|e| format!("overlay window: {e}"))?;
        // A layered window shows nothing until told how: wholly, its content's own alpha.
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA).map_err(|e| format!("layered: {e}"))?;
        super::set_corners(hwnd, 1); // DWMWCP_DONOTROUND
        let none: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(34), &none as *const _ as _, 4);
        let off: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const _ as _, 4);
        // The size was the main display's guess at the DPI; on the monitor it opened on, the
        // window has that monitor's: the app's first size request settles it.
        if opts.click_through {
            set_click_through(hwnd, true);
        }
        Ok((hwnd, place))
    }
}

/// Shows the overlay without activating it.
pub(super) fn show(hwnd: HWND) {
    // SAFETY: a handle and plain values.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// The overlay's size becomes `w`×`h` px about its bottom centre.
pub(super) fn set_size(hwnd: HWND, w: i32, h: i32) {
    let mut r = RECT::default();
    // SAFETY: `r` is a live local.
    let _ = unsafe { GetWindowRect(hwnd, &mut r) };
    if (r.right - r.left, r.bottom - r.top) == (w, h) {
        return;
    }
    let n = rect_at(place_of(r), w.max(1), h.max(1));
    // SAFETY: a handle and plain values.
    let _ = unsafe {
        SetWindowPos(hwnd, None, n.left, n.top, n.right - n.left, n.bottom - n.top, SWP_NOZORDER | SWP_NOACTIVATE)
    };
}

/// The overlay's place now.
pub(super) fn place(hwnd: HWND) -> Place {
    let mut r = RECT::default();
    // SAFETY: `r` is a live local.
    let _ = unsafe { GetWindowRect(hwnd, &mut r) };
    place_of(r)
}

/// The overlay lets the pointer through (`on`) or takes it. Through, the pointer is followed
/// by a low-level mouse hook (removed when it takes the pointer again, or the window goes).
pub(super) fn set_click_through(hwnd: HWND, on: bool) {
    // SAFETY: the window's own handle on its thread; the style is read and written whole.
    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let ex = if on { ex | WS_EX_TRANSPARENT.0 } else { ex & !WS_EX_TRANSPARENT.0 };
        SetWindowLongW(hwnd, GWL_EXSTYLE, ex as i32);
    }
    if on {
        let had = HOOK.with(|h| h.borrow().is_some());
        if !had {
            // SAFETY: the hook procedure lives as long as the process; a low-level hook takes no
            // module and runs on this thread, which pumps messages.
            let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(pointer_hook), None, 0) };
            match hook {
                Ok(hook) => HOOK.with(|h| *h.borrow_mut() = Some(PointerHook { hook, hwnd, inside: false })),
                Err(e) => eprintln!("novaui: overlay pointer hook: {e}"),
            }
        }
    } else {
        unhook();
    }
}

/// The pointer hook goes (click-through off, the window closing).
pub(super) fn unhook() {
    if let Some(h) = HOOK.with(|h| h.borrow_mut().take()) {
        // SAFETY: the hook this thread set.
        let _ = unsafe { UnhookWindowsHookEx(h.hook) };
    }
}

/// The pointer moved somewhere on the screen: over the overlay it is the overlay's move, and
/// leaving it the overlay's leave. Never swallows the move.
extern "system" fn pointer_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && wp.0 as u32 == WM_MOUSEMOVE {
        // SAFETY: for WH_MOUSE_LL, `lp` points at the event's MSLLHOOKSTRUCT for this call.
        let pt = unsafe { (*(lp.0 as *const MSLLHOOKSTRUCT)).pt };
        HOOK.with(|h| {
            let Ok(mut g) = h.try_borrow_mut() else { return };
            let Some(h) = g.as_mut() else { return };
            let mut r = RECT::default();
            // SAFETY: `r` and `c` are live locals; a stale handle only fails the calls.
            let inside = unsafe { GetWindowRect(h.hwnd, &mut r) }.is_ok()
                && (r.left..r.right).contains(&pt.x)
                && (r.top..r.bottom).contains(&pt.y);
            if inside {
                let mut c = pt;
                // SAFETY: as above.
                let _ = unsafe { ScreenToClient(h.hwnd, &mut c) };
                push(Msg::In(Input::Move(c.x, c.y)));
            } else if h.inside {
                push(Msg::In(Input::Leave));
            }
            h.inside = inside;
        });
    }
    // SAFETY: the event as Windows gave it, passed on.
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// The pointer is over the window (a leave message that comes while it still is, from the
/// caption to the client or back, is not a leave).
pub(super) fn pointer_inside(hwnd: HWND) -> bool {
    let (mut pt, mut r) = (POINT::default(), RECT::default());
    // SAFETY: live locals.
    let known = unsafe { GetCursorPos(&mut pt).is_ok() && GetWindowRect(hwnd, &mut r).is_ok() };
    known && (r.left..r.right).contains(&pt.x) && (r.top..r.bottom).contains(&pt.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An overlay grows and shrinks about its bottom centre: the place it reports is the one it
    /// was put at, whatever its size.
    #[test]
    fn the_place_is_the_bottom_centre_at_every_size() {
        let p = Place { x: 1280, y: 1260 };
        for (w, h) in [(1, 1), (600, 120), (601, 121), (2559, 300)] {
            let r = rect_at(p, w, h);
            assert_eq!((r.right - r.left, r.bottom - r.top), (w, h));
            assert_eq!(place_of(r), p, "{w}x{h}");
        }
        let left_of_origin = Place { x: -1000, y: 400 };
        assert_eq!(place_of(rect_at(left_of_origin, 333, 50)), left_of_origin);
        let work = RECT { left: 0, top: 0, right: 2560, bottom: 1400 };
        assert_eq!(default_place(work, 0.125), Place { x: 1280, y: 1225 });
    }
}
