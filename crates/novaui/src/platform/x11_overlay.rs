//! X11 overlays (`OverlayOptions`): an undecorated window on a 32-bit ARGB visual, kept above
//! the others and off the taskbar and pager (`_NET_WM_STATE_ABOVE`, `_SKIP_TASKBAR`,
//! `_SKIP_PAGER`), never given the keyboard (`WM_HINTS` input false); its place is its bottom
//! centre in root coordinates. Click-through is an empty input shape (XFixes); while it is on,
//! XInput2's raw motion on the root says where the pointer is, so the overlay still hears it
//! come and go.
//!
//! `x11.rs`'s contract holds for every `unsafe` here.

use super::X;
use crate::input::Input;
use crate::platform::{OverlayOptions, Place};
use std::os::raw::{c_int, c_long, c_uchar, c_uint, c_ulong};
use x11_dl::xlib;

/// What an overlay's host keeps beside the window.
pub(super) struct Overlay {
    /// Its bottom centre, root coordinates, as last seen.
    pub place: Place,
    fixes: Option<x11_dl::xfixes::Xlib>,
    /// XInput2 and its extension's opcode, while the pointer is followed (click-through on).
    xi: Option<(x11_dl::xinput2::XInput2, c_int)>,
    /// The pointer was over the overlay at the last raw motion.
    inside: bool,
}

/// A rectangle in root coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    fn contains(&self, x: i32, y: i32) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }

    fn intersect(&self, o: &Area) -> Option<Area> {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = ((self.x + self.w).min(o.x + o.w), (self.y + self.h).min(o.y + o.h));
        (x1 > x0 && y1 > y0).then_some(Area { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }
}

/// The top-left corner of an overlay `w`×`h` px whose bottom centre is `place`.
pub(super) fn origin(place: Place, w: i32, h: i32) -> (i32, i32) {
    (place.x - w / 2, place.y - h)
}

/// The place of an overlay at `(x, y)`, `w`×`h` px (`origin`'s inverse).
pub(super) fn place_at(x: i32, y: i32, w: i32, h: i32) -> Place {
    Place { x: x + w / 2, y: y + h }
}

/// The default place in `work`: centred at its bottom, `lift` of its height above it.
fn default_place(work: Area, lift: f32) -> Place {
    let lift = (lift.clamp(0.0, 1.0) * work.h as f32).round() as i32;
    Place { x: work.x + work.w / 2, y: work.y + work.h - lift }
}

/// The 32-bit TrueColor visual a transparent window is made on, with a colormap for it.
pub(super) unsafe fn argb_visual(
    xl: &xlib::Xlib,
    dpy: *mut xlib::Display,
    root: c_ulong,
) -> Option<(*mut xlib::Visual, c_ulong)> {
    let mut vi: xlib::XVisualInfo = std::mem::zeroed();
    if (xl.XMatchVisualInfo)(dpy, (xl.XDefaultScreen)(dpy), 32, xlib::TrueColor, &mut vi) == 0 {
        return None;
    }
    let cmap = (xl.XCreateColormap)(dpy, root, vi.visual, xlib::AllocNone);
    Some((vi.visual, cmap))
}

/// The overlay's window under `root`: on the ARGB visual, cleared to nothing, NorthWest gravity.
pub(super) unsafe fn window(
    xl: &xlib::Xlib,
    dpy: *mut xlib::Display,
    root: c_ulong,
    (x, y, w, h): (i32, i32, c_uint, c_uint),
    events: c_long,
    (visual, cmap): (*mut xlib::Visual, c_ulong),
) -> c_ulong {
    let mut attrs: xlib::XSetWindowAttributes = std::mem::zeroed();
    attrs.event_mask = events;
    attrs.background_pixel = 0;
    attrs.border_pixel = 0;
    attrs.colormap = cmap;
    attrs.bit_gravity = xlib::NorthWestGravity;
    (xl.XCreateWindow)(
        dpy,
        root,
        x,
        y,
        w,
        h,
        0,
        32,
        xlib::InputOutput as c_uint,
        visual,
        xlib::CWEventMask | xlib::CWBackPixel | xlib::CWBorderPixel | xlib::CWColormap | xlib::CWBitGravity,
        &mut attrs,
    )
}

/// The main display's rectangle (RandR's primary output), else the screen's.
unsafe fn main_display(xl: &xlib::Xlib, dpy: *mut xlib::Display, root: c_ulong) -> Area {
    let screen = (xl.XDefaultScreen)(dpy);
    let whole = Area { x: 0, y: 0, w: (xl.XDisplayWidth)(dpy, screen), h: (xl.XDisplayHeight)(dpy, screen) };
    let Ok(rr) = x11_dl::xrandr::Xrandr::open() else { return whole };
    let res = (rr.XRRGetScreenResourcesCurrent)(dpy, root);
    if res.is_null() {
        return whole;
    }
    let mut out = whole;
    let primary = (rr.XRRGetOutputPrimary)(dpy, root);
    let outputs = std::slice::from_raw_parts((*res).outputs, (*res).noutput.max(0) as usize);
    // The primary output, else the first one lit.
    let order = std::iter::once(primary).filter(|o| *o != 0).chain(outputs.iter().copied());
    for output in order {
        let info = (rr.XRRGetOutputInfo)(dpy, res, output);
        if info.is_null() {
            continue;
        }
        let crtc = (*info).crtc;
        (rr.XRRFreeOutputInfo)(info);
        if crtc == 0 {
            continue;
        }
        let c = (rr.XRRGetCrtcInfo)(dpy, res, crtc);
        if c.is_null() {
            continue;
        }
        let a = Area { x: (*c).x, y: (*c).y, w: (*c).width as i32, h: (*c).height as i32 };
        (rr.XRRFreeCrtcInfo)(c);
        if a.w > 0 && a.h > 0 {
            out = a;
            break;
        }
    }
    (rr.XRRFreeScreenResources)(res);
    out
}

/// The work area the window manager publishes (`_NET_WORKAREA`, the current desktop's), if any.
unsafe fn net_workarea(x: &X) -> Option<Area> {
    let prop = X::atom(&x.xl, x.dpy, "_NET_WORKAREA");
    let (mut ty, mut fmt, mut n, mut rest) = (0, 0, 0, 0);
    let mut data: *mut c_uchar = std::ptr::null_mut();
    let ok = (x.xl.XGetWindowProperty)(
        x.dpy,
        x.root,
        prop,
        0,
        4,
        0,
        xlib::XA_CARDINAL,
        &mut ty,
        &mut fmt,
        &mut n,
        &mut rest,
        &mut data,
    );
    if ok != 0 || data.is_null() {
        return None;
    }
    let v = (n >= 4).then(|| std::slice::from_raw_parts(data as *const c_long, 4).to_vec());
    (x.xl.XFree)(data as *mut _);
    let v = v?;
    Some(Area { x: v[0] as i32, y: v[1] as i32, w: v[2] as i32, h: v[3] as i32 })
}

/// The main display's work area: the display, less what the window manager keeps for panels.
pub(super) unsafe fn work_area(x: &X) -> Area {
    let main = main_display(&x.xl, x.dpy, x.root);
    net_workarea(x).and_then(|w| w.intersect(&main)).unwrap_or(main)
}

/// Where the overlay opens: the kept place when it is on the screen, else the default.
pub(super) unsafe fn opening_place(x: &X, opts: &OverlayOptions) -> Place {
    let screen = (x.xl.XDefaultScreen)(x.dpy);
    let whole = Area { x: 0, y: 0, w: (x.xl.XDisplayWidth)(x.dpy, screen), h: (x.xl.XDisplayHeight)(x.dpy, screen) };
    match opts.place {
        Some(p) if whole.contains(p.x, p.y - 1) => p,
        _ => default_place(work_area(x), opts.lift),
    }
}

/// The overlay's properties before it is mapped: above the others, off the taskbar and pager,
/// a utility window, never given the keyboard, at its place.
pub(super) unsafe fn set_properties(x: &X, (left, top): (i32, i32), (w, h): (i32, i32)) {
    let state = X::atom(&x.xl, x.dpy, "_NET_WM_STATE");
    let states = ["_NET_WM_STATE_ABOVE", "_NET_WM_STATE_SKIP_TASKBAR", "_NET_WM_STATE_SKIP_PAGER"]
        .map(|n| X::atom(&x.xl, x.dpy, n) as c_long);
    (x.xl.XChangeProperty)(
        x.dpy,
        x.win,
        state,
        xlib::XA_ATOM,
        32,
        xlib::PropModeReplace,
        states.as_ptr() as *const c_uchar,
        states.len() as c_int,
    );
    let ty = X::atom(&x.xl, x.dpy, "_NET_WM_WINDOW_TYPE");
    let utility = X::atom(&x.xl, x.dpy, "_NET_WM_WINDOW_TYPE_UTILITY") as c_long;
    (x.xl.XChangeProperty)(
        x.dpy,
        x.win,
        ty,
        xlib::XA_ATOM,
        32,
        xlib::PropModeReplace,
        &utility as *const c_long as *const c_uchar,
        1,
    );
    let hints = (x.xl.XAllocWMHints)();
    if !hints.is_null() {
        (*hints).flags = xlib::InputHint;
        (*hints).input = 0;
        (x.xl.XSetWMHints)(x.dpy, x.win, hints);
        (x.xl.XFree)(hints as *mut _);
    }
    // The place is the program's: window managers keep a position the user or program gave.
    let mut sh: xlib::XSizeHints = std::mem::zeroed();
    sh.flags = xlib::USPosition | xlib::PPosition;
    sh.x = left;
    sh.y = top;
    sh.width = w;
    sh.height = h;
    (x.xl.XSetWMNormalHints)(x.dpy, x.win, &mut sh);
}

/// The window's top-left corner in root coordinates (a reparenting window manager's frame
/// between them included).
pub(super) unsafe fn root_origin(x: &X) -> (i32, i32) {
    let (mut rx, mut ry, mut child) = (0, 0, 0);
    (x.xl.XTranslateCoordinates)(x.dpy, x.win, x.root, 0, 0, &mut rx, &mut ry, &mut child);
    (rx, ry)
}

impl Overlay {
    pub(super) fn new(place: Place) -> Overlay {
        Overlay { place, fixes: x11_dl::xfixes::Xlib::open().ok(), xi: None, inside: false }
    }

    /// The overlay at `w`×`h` px about its place.
    pub(super) unsafe fn set_size(&self, x: &X, w: i32, h: i32) {
        let (left, top) = origin(self.place, w.max(1), h.max(1));
        (x.xl.XMoveResizeWindow)(x.dpy, x.win, left, top, w.max(1) as c_uint, h.max(1) as c_uint);
    }

    /// The window was configured: its place now, `Some` when that moved.
    pub(super) unsafe fn configured(&mut self, x: &X, (w, h): (i32, i32)) -> Option<Place> {
        let (left, top) = root_origin(x);
        let place = place_at(left, top, w, h);
        (place != self.place).then(|| {
            self.place = place;
            place
        })
    }

    /// Presses go through it (`on`: an empty input shape) or come to it; through, the pointer
    /// is followed by XInput2's raw motion on the root.
    pub(super) unsafe fn set_click_through(&mut self, x: &X, on: bool) {
        let Some(fixes) = &self.fixes else {
            eprintln!("novaui: overlay: no XFixes, so no click-through");
            return;
        };
        // ShapeInput.
        const SHAPE_INPUT: c_int = 2;
        if on {
            let region = (fixes.XFixesCreateRegion)(x.dpy, std::ptr::null_mut(), 0);
            (fixes.XFixesSetWindowShapeRegion)(x.dpy, x.win, SHAPE_INPUT, 0, 0, region);
            (fixes.XFixesDestroyRegion)(x.dpy, region);
        } else {
            // No region: the window's own shape again.
            (fixes.XFixesSetWindowShapeRegion)(x.dpy, x.win, SHAPE_INPUT, 0, 0, 0);
        }
        self.follow_pointer(x, on);
    }

    unsafe fn follow_pointer(&mut self, x: &X, on: bool) {
        if on && self.xi.is_none() {
            let Ok(xi) = x11_dl::xinput2::XInput2::open() else { return };
            let (mut opcode, mut event, mut error) = (0, 0, 0);
            if (x.xl.XQueryExtension)(x.dpy, c"XInputExtension".as_ptr(), &mut opcode, &mut event, &mut error) == 0 {
                return;
            }
            let (mut major, mut minor) = (2, 0);
            if (xi.XIQueryVersion)(x.dpy, &mut major, &mut minor) != 0 {
                return;
            }
            self.select_raw(x, &xi, x11_dl::xinput2::XI_RawMotionMask);
            self.xi = Some((xi, opcode));
        } else if !on {
            if let Some((xi, _)) = self.xi.take() {
                self.select_raw(x, &xi, 0);
            }
            self.inside = false;
        }
    }

    unsafe fn select_raw(&self, x: &X, xi: &x11_dl::xinput2::XInput2, bits: i32) {
        let mut bytes = bits.to_le_bytes();
        let mut mask = x11_dl::xinput2::XIEventMask {
            deviceid: x11_dl::xinput2::XIAllMasterDevices,
            mask_len: bytes.len() as c_int,
            mask: bytes.as_mut_ptr(),
        };
        (xi.XISelectEvents)(x.dpy, x.root, &mut mask, 1);
    }

    /// A generic event: when it is the pointer's raw motion while the overlay lets it through,
    /// the overlay's move (over it) or leave (off it).
    pub(super) unsafe fn raw_motion(&mut self, x: &X, ev: &mut xlib::XEvent, (w, h): (i32, i32)) -> Option<Input> {
        let (_, opcode) = self.xi.as_ref()?;
        let cookie = &mut ev.generic_event_cookie;
        if cookie.extension != *opcode || cookie.evtype != x11_dl::xinput2::XI_RawMotion {
            return None;
        }
        // The raw event carries no position: the pointer is asked where it is.
        let (mut root, mut child) = (0 as c_ulong, 0 as c_ulong);
        let (mut rx, mut ry, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0);
        (x.xl.XQueryPointer)(x.dpy, x.win, &mut root, &mut child, &mut rx, &mut ry, &mut wx, &mut wy, &mut mask);
        let inside = (0..w).contains(&wx) && (0..h).contains(&wy);
        let was = std::mem::replace(&mut self.inside, inside);
        if inside {
            Some(Input::Move(wx, wy))
        } else {
            was.then_some(Input::Leave)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The place is the bottom centre at every size, and the default stands an eighth up.
    #[test]
    fn the_place_is_the_bottom_centre_at_every_size() {
        let p = Place { x: 960, y: 900 };
        for (w, h) in [(1, 1), (600, 120), (601, 121)] {
            let (x, y) = origin(p, w, h);
            assert_eq!(place_at(x, y, w, h), p, "{w}x{h}");
        }
        let work = Area { x: 1920, y: 0, w: 1920, h: 1040 };
        assert_eq!(default_place(work, 0.125), Place { x: 2880, y: 910 });
        let main = Area { x: 0, y: 0, w: 1920, h: 1080 };
        assert_eq!(Area { x: 0, y: 0, w: 3840, h: 1040 }.intersect(&main), Some(Area { x: 0, y: 0, w: 1920, h: 1040 }));
    }
}
