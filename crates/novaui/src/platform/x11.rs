//! X11 host (Xlib, loaded at run time). One undecorated window: the app draws its title bar and
//! the window manager moves/resizes it through `_NET_WM_MOVERESIZE`. Input is translated into
//! `ui::Input` with Windows virtual-key codes (the kernel's key vocabulary). The loop blocks in
//! `poll` on the X connection and a wake pipe until input, a wake or the next scheduled frame.
//! Frame pacing is the swap chain's: FIFO presentation blocks once the GPU is a frame ahead.
//!
//! The contract every `unsafe` block here relies on unless its comment says more: Xlib is called
//! only from the thread that runs the window, through function pointers of the library loaded
//! for the whole run; the display connection and the window live until `run` returns, and
//! nothing that holds them (backend, presenter) outlives it; every pointer passed to Xlib is a
//! live local or an Xlib object not yet freed, and every object Xlib returns is freed once.

use super::linux::{border_dir, pointer_taken, vk, CaptionPress, HostWake};
use super::xim::Xim;
use super::{App, Options, OverlayOptions, Place};
use crate::app::{NcHit, Ui};
use crate::geom::Rect;
use crate::input::{Input, Mods, MouseButton};
use crate::render::primitive::Backend;
use crate::runner::{self, Pacer};
use crate::style::Cursor;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;
use x11_dl::xlib;

#[path = "x11_overlay.rs"]
mod overlay;

/// The events the window takes (an input method's own are added, `Xim`).
const EVENT_MASK: c_long = xlib::ExposureMask
    | xlib::StructureNotifyMask
    | xlib::KeyPressMask
    | xlib::KeyReleaseMask
    | xlib::ButtonPressMask
    | xlib::ButtonReleaseMask
    | xlib::PointerMotionMask
    | xlib::LeaveWindowMask
    | xlib::FocusChangeMask
    | xlib::PropertyChangeMask;

struct Atoms {
    wm_protocols: c_ulong,
    wm_delete: c_ulong,
    net_wm_state: c_ulong,
    max_h: c_ulong,
    max_v: c_ulong,
    fullscreen: c_ulong,
    moveresize: c_ulong,
    motif_hints: c_ulong,
    clipboard: c_ulong,
    utf8: c_ulong,
    targets: c_ulong,
    paste_prop: c_ulong,
    net_wm_name: c_ulong,
    sync_request: c_ulong,
    window_menu: c_ulong,
}

struct X {
    xl: xlib::Xlib,
    dpy: *mut xlib::Display,
    win: c_ulong,
    root: c_ulong,
    atoms: Atoms,
    /// The input method (`xim.rs`): fcitx5, IBus.
    xim: Xim,
    cursors: [c_ulong; 5],
    /// Resize cursors by `_NET_WM_MOVERESIZE` direction (0 top-left ... 7 left).
    edge_cursors: [c_ulong; 8],
    clipboard: Option<String>,
    /// `_NET_WM_SYNC_REQUEST`: the WM waits for this counter before the next resize step, so a
    /// live resize goes at the speed we draw instead of queueing sizes ahead of us.
    sync: Option<(x11_dl::sync::Xext, x11_dl::sync::XSyncCounter)>,
    /// The Vulkan surface's child window at (0,0), sized by `vk::surface_fit`: larger than the
    /// window in 256-px steps (the parent clips it), so most live-resize steps reuse the swap
    /// chain. It selects only Expose: input propagates to the parent.
    pres: c_ulong,
    pres_size: (i32, i32),
    /// The window manager answers `_GTK_SHOW_WINDOW_MENU` (KWin, Mutter, Xfwm list it).
    window_menu: bool,
    /// An overlay's ARGB visual, and the backend it was asked for.
    argb: Option<*mut xlib::Visual>,
    backend: super::BackendKind,
}

/// Software presentation on X11: the reference rasterizer, its damage put with XPutImage.
struct SoftwareX11 {
    sw: crate::render::software::Software,
    dpy: *mut xlib::Display,
    win: c_ulong,
    gc: xlib::GC,
    put: unsafe extern "C" fn(
        *mut xlib::Display,
        c_ulong,
        xlib::GC,
        *mut xlib::XImage,
        c_int,
        c_int,
        c_int,
        c_int,
        c_uint,
        c_uint,
    ) -> c_int,
    create: unsafe extern "C" fn(
        *mut xlib::Display,
        *mut xlib::Visual,
        c_uint,
        c_int,
        c_int,
        *mut c_char,
        c_uint,
        c_uint,
        c_int,
        c_int,
    ) -> *mut xlib::XImage,
    visual: *mut xlib::Visual,
    /// 24 (the default visual's), or 32 for an overlay's ARGB window: the canvas's premultiplied
    /// 0xAARRGGBB is what that visual takes.
    depth: c_uint,
    flush: unsafe extern "C" fn(*mut xlib::Display) -> c_int,
}

impl Backend for SoftwareX11 {
    fn name(&self) -> &'static str {
        "sw"
    }
    fn resize(&mut self, w: i32, h: i32) {
        self.sw.resize(w, h)
    }
    fn upload_atlas(&mut self, rect: Rect, atlas: &[u8], atlas_w: i32) {
        self.sw.upload_atlas(rect, atlas, atlas_w)
    }
    fn atlas_size(&self) -> i32 {
        self.sw.atlas_size()
    }
    fn layer_alloc(&mut self, id: u32, w: i32, h: i32) {
        self.sw.layer_alloc(id, w, h)
    }
    fn layer_free(&mut self, id: u32) {
        self.sw.layer_free(id)
    }
    fn image_upload(&mut self, id: u32, w: i32, h: i32, rgba: &[u8]) {
        self.sw.image_upload(id, w, h, rgba)
    }
    fn image_free(&mut self, id: u32) {
        self.sw.image_free(id)
    }
    fn draw(
        &mut self,
        target: crate::render::primitive::Target,
        clip: Rect,
        offset: (i32, i32),
        clear: crate::geom::Color,
        draws: &[crate::render::primitive::Draw],
    ) {
        self.sw.draw(target, clip, offset, clear, draws)
    }
    fn present(&mut self, damage: &[Rect]) {
        let c = &mut self.sw.canvas;
        // SAFETY: the module contract; the image borrows the canvas's w×h pixels for this call only
        // (detached before it is destroyed), and each put is clipped to the canvas.
        unsafe {
            // A 24-bit TrueColor visual stores 0x00RRGGBB little-endian, a 32-bit one 0xAARRGGBB:
            // the canvas's own layout.
            let img = (self.create)(
                self.dpy,
                self.visual,
                self.depth,
                xlib::ZPixmap,
                0,
                c.px.as_mut_ptr() as *mut c_char,
                c.w as c_uint,
                c.h as c_uint,
                32,
                0,
            );
            if img.is_null() {
                return;
            }
            for r in damage {
                let r = r.intersect(&Rect::new(0, 0, c.w, c.h));
                if !r.is_empty() {
                    (self.put)(self.dpy, self.win, self.gc, img, r.x, r.y, r.x, r.y, r.w as c_uint, r.h as c_uint);
                }
            }
            // The pixels are the canvas's: detach them before XDestroyImage's free.
            destroy_borrowed_image(img);
            (self.flush)(self.dpy);
        }
    }
    fn readback(&mut self) -> Vec<u32> {
        self.sw.readback()
    }
}

fn make_backend(kind: super::BackendKind, x: &X, w: i32, h: i32) -> Box<dyn Backend> {
    if kind != super::BackendKind::Software {
        match crate::render::vulkan::Vulkan::new(w, h) {
            Ok(mut v) => {
                match v.attach(crate::render::vulkan::Window::Xlib { display: x.dpy as *mut _, window: x.pres }, w, h) {
                    Ok(()) => return Box::new(v),
                    Err(e) => eprintln!("novaui: Vulkan surface unavailable ({e}); software fallback"),
                }
            }
            Err(e) => eprintln!("novaui: Vulkan unavailable ({e}); software fallback"),
        }
    }
    // SAFETY: the module contract.
    unsafe {
        // Software draws into the window itself: the Vulkan child must not cover it.
        (x.xl.XUnmapWindow)(x.dpy, x.pres);
        let screen = (x.xl.XDefaultScreen)(x.dpy);
        Box::new(SoftwareX11 {
            sw: crate::render::software::Software::new(w, h),
            dpy: x.dpy,
            win: x.win,
            gc: (x.xl.XDefaultGC)(x.dpy, screen),
            put: x.xl.XPutImage,
            create: x.xl.XCreateImage,
            visual: (x.xl.XDefaultVisual)(x.dpy, screen),
            depth: 24,
            flush: x.xl.XFlush,
        })
    }
}

/// An overlay's backend: Vulkan composing the canvas's alpha, else (no Vulkan, a surface that
/// composes no alpha: NVIDIA's) the software canvas put on the ARGB window as it is.
fn transparent_backend(x: &X, w: i32, h: i32) -> Result<Box<dyn Backend>, String> {
    let vulkan = || -> Result<Box<dyn Backend>, String> {
        let mut v = crate::render::vulkan::Vulkan::new(w, h)?;
        v.set_transparent(true);
        v.attach(crate::render::vulkan::Window::Xlib { display: x.dpy as *mut _, window: x.pres }, w, h)?;
        if !v.composes_alpha() {
            return Err("the Vulkan surface composes no alpha".into());
        }
        Ok(Box::new(v))
    };
    if x.backend != super::BackendKind::Software {
        match vulkan() {
            Ok(be) => return Ok(be),
            Err(e) => eprintln!("novaui: overlay: {e}; software presentation"),
        }
    }
    let visual = x.argb.ok_or("no 32-bit visual")?;
    // SAFETY: the module contract; the GC is the window's (its depth), freed with the display.
    unsafe {
        (x.xl.XUnmapWindow)(x.dpy, x.pres);
        Ok(Box::new(SoftwareX11 {
            sw: crate::render::software::Software::new(w, h),
            dpy: x.dpy,
            win: x.win,
            gc: (x.xl.XCreateGC)(x.dpy, x.win, 0, std::ptr::null_mut()),
            put: x.xl.XPutImage,
            create: x.xl.XCreateImage,
            visual,
            depth: 32,
            flush: x.xl.XFlush,
        }))
    }
}

/// A `w`×`h` window under `parent` taking `events`, with no background and NorthWest bit gravity.
/// The X server would clear a window to its background on every resize, before our frame at the
/// new size lands (a black flash per step of a live resize); the gravity keeps the old contents
/// in place instead. The top-level window and the Vulkan child are made so.
/// Kept bounds a window can open at on a screen of `size`: their top strip (where it is dragged
/// from) on it by at least 120 × 16 pixels, and no bigger than it.
fn reachable(b: &super::Bounds, (sw, sh): (c_int, c_int)) -> bool {
    let (l, r) = (b.x.max(0), (b.x + b.width).min(sw));
    let (t, bt) = (b.y.max(0), (b.y + 32).min(sh));
    r - l >= 120 && bt - t >= 16 && b.width <= sw && b.height <= sh
}

unsafe fn plain_window(
    xl: &xlib::Xlib,
    dpy: *mut xlib::Display,
    parent: c_ulong,
    (w, h): (c_uint, c_uint),
    events: c_long,
) -> c_ulong {
    let mut attrs: xlib::XSetWindowAttributes = std::mem::zeroed();
    attrs.event_mask = events;
    attrs.background_pixmap = 0;
    attrs.bit_gravity = xlib::NorthWestGravity;
    (xl.XCreateWindow)(
        dpy,
        parent,
        0,
        0,
        w,
        h,
        0,
        xlib::CopyFromParent,
        xlib::InputOutput as c_uint,
        std::ptr::null_mut(),
        xlib::CWEventMask | xlib::CWBackPixmap | xlib::CWBitGravity,
        &mut attrs,
    )
}

/// Xft.dpi from the resource database (what KDE/GNOME set for scaling), else 96.
unsafe fn dpi_scale(xl: &xlib::Xlib, dpy: *mut xlib::Display) -> f32 {
    let s = (xl.XResourceManagerString)(dpy);
    if !s.is_null() {
        for line in CStr::from_ptr(s).to_string_lossy().lines() {
            if let Some(v) = line.strip_prefix("Xft.dpi:") {
                if let Ok(d) = v.trim().parse::<f32>() {
                    return (d / 96.0).max(1.0);
                }
            }
        }
    }
    1.0
}

fn mods(state: c_uint) -> Mods {
    Mods { shift: state & xlib::ShiftMask != 0, ctrl: state & xlib::ControlMask != 0, alt: state & xlib::Mod1Mask != 0 }
}

impl X {
    unsafe fn atom(xl: &xlib::Xlib, dpy: *mut xlib::Display, name: &str) -> c_ulong {
        let c = CString::new(name).expect("atom names are literals without NUL");
        (xl.XInternAtom)(dpy, c.as_ptr(), 0)
    }

    /// Opens the display and makes the window (hidden): `overlay`, an overlay's at its place
    /// (returned with it).
    unsafe fn open(opts: &Options, overlay: Option<&OverlayOptions>) -> Result<(X, Option<Place>), String> {
        libc::setlocale(libc::LC_CTYPE, c"".as_ptr());
        let xl = xlib::Xlib::open().map_err(|e| format!("libX11: {e}"))?;
        (xl.XInitThreads)();
        (xl.XSetLocaleModifiers)(c"".as_ptr());
        let dpy = (xl.XOpenDisplay)(std::ptr::null());
        if dpy.is_null() {
            return Err("cannot open X display (DISPLAY unset?)".into());
        }
        let screen = (xl.XDefaultScreen)(dpy);
        let root = (xl.XRootWindow)(dpy, screen);
        let scale = dpi_scale(&xl, dpy);
        // Where it last stood (`Options::bounds`), if its top strip is still on the screen.
        let screen_size = ((xl.XDisplayWidth)(dpy, screen), (xl.XDisplayHeight)(dpy, screen));
        let kept = opts.bounds.filter(|b| overlay.is_none() && reachable(b, screen_size));
        let (w, h) = match kept {
            Some(b) => (b.width as u32, b.height as u32),
            None => ((opts.width as f32 * scale) as u32, (opts.height as f32 * scale) as u32),
        };
        let argb = match overlay {
            Some(_) => Some(overlay::argb_visual(&xl, dpy, root).ok_or("no 32-bit visual for a transparent window")?),
            None => None,
        };
        let win = match argb {
            Some(v) => overlay::window(&xl, dpy, root, (0, 0, w, h), EVENT_MASK, v),
            None => plain_window(&xl, dpy, root, (w, h), EVENT_MASK),
        };
        let atoms = Atoms {
            wm_protocols: Self::atom(&xl, dpy, "WM_PROTOCOLS"),
            wm_delete: Self::atom(&xl, dpy, "WM_DELETE_WINDOW"),
            net_wm_state: Self::atom(&xl, dpy, "_NET_WM_STATE"),
            max_h: Self::atom(&xl, dpy, "_NET_WM_STATE_MAXIMIZED_HORZ"),
            max_v: Self::atom(&xl, dpy, "_NET_WM_STATE_MAXIMIZED_VERT"),
            fullscreen: Self::atom(&xl, dpy, "_NET_WM_STATE_FULLSCREEN"),
            moveresize: Self::atom(&xl, dpy, "_NET_WM_MOVERESIZE"),
            motif_hints: Self::atom(&xl, dpy, "_MOTIF_WM_HINTS"),
            clipboard: Self::atom(&xl, dpy, "CLIPBOARD"),
            utf8: Self::atom(&xl, dpy, "UTF8_STRING"),
            targets: Self::atom(&xl, dpy, "TARGETS"),
            paste_prop: Self::atom(&xl, dpy, "NOVAUI_PASTE"),
            net_wm_name: Self::atom(&xl, dpy, "_NET_WM_NAME"),
            sync_request: Self::atom(&xl, dpy, "_NET_WM_SYNC_REQUEST"),
            window_menu: Self::atom(&xl, dpy, "_GTK_SHOW_WINDOW_MENU"),
        };
        let sync = x11_dl::sync::Xext::open().ok().and_then(|xe| {
            let (mut ev, mut er, mut maj, mut min) = (0, 0, 0, 0);
            if (xe.XSyncQueryExtension)(dpy, &mut ev, &mut er) == 0
                || (xe.XSyncInitialize)(dpy, &mut maj, &mut min) == 0
            {
                return None;
            }
            let c = (xe.XSyncCreateCounter)(dpy, x11_dl::sync::XSyncValue { hi: 0, lo: 0 });
            let v = c as c_long;
            let prop = Self::atom(&xl, dpy, "_NET_WM_SYNC_REQUEST_COUNTER");
            (xl.XChangeProperty)(
                dpy,
                win,
                prop,
                xlib::XA_CARDINAL,
                32,
                xlib::PropModeReplace,
                &v as *const c_long as *const c_uchar,
                1,
            );
            Some((xe, c))
        });
        let mut protos = vec![atoms.wm_delete];
        if sync.is_some() {
            protos.push(atoms.sync_request);
        }
        (xl.XSetWMProtocols)(dpy, win, protos.as_mut_ptr(), protos.len() as c_int);
        if kept.is_some_and(|b| b.maximized) {
            // Maximized from the first map (EWMH: the state set before the window is mapped).
            let state = [atoms.max_h, atoms.max_v];
            (xl.XChangeProperty)(
                dpy,
                win,
                atoms.net_wm_state,
                xlib::XA_ATOM,
                32,
                xlib::PropModeReplace,
                state.as_ptr() as *const c_uchar,
                2,
            );
        }
        // No WM decorations: the app's title bar is the caption (Motif hints: flags=2, decorations=0).
        let hints: [c_long; 5] = [2, 0, 0, 0, 0];
        (xl.XChangeProperty)(
            dpy,
            win,
            atoms.motif_hints,
            atoms.motif_hints,
            32,
            xlib::PropModeReplace,
            hints.as_ptr() as *const c_uchar,
            5,
        );
        if overlay.is_none() {
            let mut sh: xlib::XSizeHints = std::mem::zeroed();
            sh.flags = xlib::PMinSize;
            sh.min_width = (opts.min_width as f32 * scale) as c_int;
            sh.min_height = (opts.min_height as f32 * scale) as c_int;
            if let Some(b) = kept {
                // The user's own place and size (ICCCM's USPosition / USSize): the window
                // manager keeps them rather than placing it.
                (xl.XMoveWindow)(dpy, win, b.x, b.y);
                sh.flags |= xlib::USPosition | xlib::USSize;
                (sh.x, sh.y, sh.width, sh.height) = (b.x, b.y, b.width, b.height);
            }
            (xl.XSetWMNormalHints)(dpy, win, &mut sh);
        }
        set_title(&xl, dpy, win, &atoms, opts.title.clone());
        // An overlay never has the keyboard: no input method.
        let xim = if overlay.is_some() { Xim::none(EVENT_MASK) } else { Xim::new(&xl, dpy, win, EVENT_MASK) };
        // XC_left_ptr, XC_hand2, XC_xterm, XC_fleur, XC_sb_v_double_arrow.
        let cursors = [68, 60, 152, 52, 116].map(|c| (xl.XCreateFontCursor)(dpy, c));
        // XC_top_left_corner, top_side, top_right_corner, right_side, bottom_right_corner,
        // bottom_side, bottom_left_corner, left_side.
        let edge_cursors = [134, 138, 136, 96, 14, 16, 12, 70].map(|c| (xl.XCreateFontCursor)(dpy, c));
        let pres_size = crate::render::vulkan::surface_fit((0, 0), (w as i32, h as i32));
        let pres = plain_window(&xl, dpy, win, (pres_size.0 as c_uint, pres_size.1 as c_uint), xlib::ExposureMask);
        (xl.XMapWindow)(dpy, pres);
        let mut x = X {
            xl,
            dpy,
            win,
            root,
            atoms,
            xim,
            cursors,
            edge_cursors,
            clipboard: None,
            sync,
            pres,
            pres_size,
            window_menu: false,
            argb: argb.map(|(v, _)| v),
            backend: opts.backend,
        };
        let supported = Self::atom(&x.xl, dpy, "_NET_SUPPORTED");
        x.window_menu = overlay.is_none() && x.atom_list(root, supported, 4096).contains(&x.atoms.window_menu);
        let place = overlay.map(|o| {
            let place = overlay::opening_place(&x, o);
            let (left, top) = overlay::origin(place, w as i32, h as i32);
            (x.xl.XMoveWindow)(dpy, win, left, top);
            overlay::set_properties(&x, (left, top), (w as i32, h as i32));
            place
        });
        Ok((x, place))
    }

    /// The window's bounds now (`Window::bounds`): its rectangle on the root while not
    /// maximized, else the one it had (`was`), with whether it is maximized.
    unsafe fn bounds(&self, was: Option<super::Bounds>) -> Option<super::Bounds> {
        if self.is_maximized() {
            return was.map(|b| super::Bounds { maximized: true, ..b });
        }
        let (w, h) = self.size();
        let (mut x, mut y, mut child) = (0, 0, 0);
        (self.xl.XTranslateCoordinates)(self.dpy, self.win, self.root, 0, 0, &mut x, &mut y, &mut child);
        Some(super::Bounds { x, y, width: w, height: h, maximized: false })
    }

    unsafe fn size(&self) -> (i32, i32) {
        let mut a: xlib::XWindowAttributes = std::mem::zeroed();
        (self.xl.XGetWindowAttributes)(self.dpy, self.win, &mut a);
        (a.width, a.height)
    }

    /// A request about this window to the window manager: a client message to the root.
    unsafe fn tell_wm(&self, message_type: c_ulong, data: [c_long; 5]) {
        let mut ev: xlib::XClientMessageEvent = std::mem::zeroed();
        ev.type_ = xlib::ClientMessage;
        ev.window = self.win;
        ev.message_type = message_type;
        ev.format = 32;
        for (i, d) in data.into_iter().enumerate() {
            ev.data.set_long(i, d);
        }
        let mut e = xlib::XEvent { client_message: ev };
        (self.xl.XSendEvent)(
            self.dpy,
            self.root,
            0,
            xlib::SubstructureRedirectMask | xlib::SubstructureNotifyMask,
            &mut e,
        );
    }

    unsafe fn wm_state(&self, add: bool, a: c_ulong, b: c_ulong) {
        self.tell_wm(self.atoms.net_wm_state, [add as c_long, a as c_long, b as c_long, 1, 0]);
    }

    /// An atom-list property of `win` (at most `max` atoms).
    unsafe fn atom_list(&self, win: c_ulong, prop: c_ulong, max: c_long) -> Vec<c_ulong> {
        let (mut ty, mut fmt, mut n, mut rest) = (0, 0, 0, 0);
        let mut data: *mut c_uchar = std::ptr::null_mut();
        let ok = (self.xl.XGetWindowProperty)(
            self.dpy,
            win,
            prop,
            0,
            max,
            0,
            xlib::XA_ATOM,
            &mut ty,
            &mut fmt,
            &mut n,
            &mut rest,
            &mut data,
        );
        let mut atoms = Vec::new();
        if ok == 0 && !data.is_null() {
            atoms.extend_from_slice(std::slice::from_raw_parts(data as *const c_ulong, n as usize));
            (self.xl.XFree)(data as *mut _);
        }
        atoms
    }

    unsafe fn is_maximized(&self) -> bool {
        let atoms = self.atom_list(self.win, self.atoms.net_wm_state, 64);
        atoms.contains(&self.atoms.max_h) && atoms.contains(&self.atoms.max_v)
    }

    /// `(x, y)` in root coordinates, with the pointer let go so the window manager can take it.
    unsafe fn release_pointer_at(&self, x: i32, y: i32) -> (c_long, c_long) {
        let (mut rx, mut ry, mut child) = (0, 0, 0);
        (self.xl.XTranslateCoordinates)(self.dpy, self.win, self.root, x, y, &mut rx, &mut ry, &mut child);
        (self.xl.XUngrabPointer)(self.dpy, xlib::CurrentTime);
        (rx as c_long, ry as c_long)
    }

    /// Hands a press to the window manager as a move (direction 8) or an edge resize (0..7).
    unsafe fn moveresize(&self, x: i32, y: i32, dir: c_long) {
        let (rx, ry) = self.release_pointer_at(x, y);
        // Button 1; source 1, a normal application.
        self.tell_wm(self.atoms.moveresize, [rx, ry, dir, 1, 1]);
        (self.xl.XFlush)(self.dpy);
    }

    /// Opens the window manager's window menu at `(x, y)`. False when it offers none.
    unsafe fn show_window_menu(&self, x: i32, y: i32) -> bool {
        if !self.window_menu {
            return false;
        }
        let (rx, ry) = self.release_pointer_at(x, y);
        // The device id first, which window managers do not need.
        self.tell_wm(self.atoms.window_menu, [0, rx, ry, 0, 0]);
        (self.xl.XFlush)(self.dpy);
        true
    }

    unsafe fn set_clipboard(&mut self, s: String) {
        self.clipboard = Some(s);
        (self.xl.XSetSelectionOwner)(self.dpy, self.atoms.clipboard, self.win, xlib::CurrentTime);
    }

    /// Answers another client's paste of our CLIPBOARD.
    unsafe fn selection_request(&self, r: &xlib::XSelectionRequestEvent) {
        let mut reply: xlib::XSelectionEvent = std::mem::zeroed();
        reply.type_ = xlib::SelectionNotify;
        reply.requestor = r.requestor;
        reply.selection = r.selection;
        reply.target = r.target;
        reply.time = r.time;
        reply.property = 0;
        if let Some(text) = &self.clipboard {
            if r.target == self.atoms.targets {
                let t = [self.atoms.utf8, xlib::XA_STRING];
                (self.xl.XChangeProperty)(
                    self.dpy,
                    r.requestor,
                    r.property,
                    xlib::XA_ATOM,
                    32,
                    xlib::PropModeReplace,
                    t.as_ptr() as *const c_uchar,
                    2,
                );
                reply.property = r.property;
            } else if r.target == self.atoms.utf8 || r.target == xlib::XA_STRING {
                (self.xl.XChangeProperty)(
                    self.dpy,
                    r.requestor,
                    r.property,
                    r.target,
                    8,
                    xlib::PropModeReplace,
                    text.as_ptr(),
                    text.len() as c_int,
                );
                reply.property = r.property;
            }
        }
        let mut e = xlib::XEvent { selection: reply };
        (self.xl.XSendEvent)(self.dpy, r.requestor, 0, 0, &mut e);
    }

    /// Reads CLIPBOARD as UTF-8, waiting up to 200 ms for the owner.
    unsafe fn paste(&mut self) -> Option<String> {
        if let Some(s) = &self.clipboard {
            if (self.xl.XGetSelectionOwner)(self.dpy, self.atoms.clipboard) == self.win {
                return Some(s.clone());
            }
        }
        (self.xl.XConvertSelection)(
            self.dpy,
            self.atoms.clipboard,
            self.atoms.utf8,
            self.atoms.paste_prop,
            self.win,
            xlib::CurrentTime,
        );
        (self.xl.XFlush)(self.dpy);
        let t0 = Instant::now();
        let mut ev: xlib::XEvent = std::mem::zeroed();
        while t0.elapsed().as_millis() < 200 {
            if (self.xl.XCheckTypedWindowEvent)(self.dpy, self.win, xlib::SelectionNotify, &mut ev) != 0 {
                if ev.selection.property == 0 {
                    return None;
                }
                let (mut ty, mut fmt, mut n, mut rest) = (0, 0, 0, 0);
                let mut data: *mut c_uchar = std::ptr::null_mut();
                (self.xl.XGetWindowProperty)(
                    self.dpy,
                    self.win,
                    self.atoms.paste_prop,
                    0,
                    1 << 20,
                    1,
                    xlib::AnyPropertyType as c_ulong,
                    &mut ty,
                    &mut fmt,
                    &mut n,
                    &mut rest,
                    &mut data,
                );
                if data.is_null() {
                    return None;
                }
                let s = String::from_utf8_lossy(std::slice::from_raw_parts(data, n as usize)).into_owned();
                (self.xl.XFree)(data as *mut _);
                return Some(s);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        None
    }
}

/// KWin's shadow for a window without decorations (`_KDE_NET_WM_SHADOW`): eight ARGB pixmaps
/// (top, top-right, right, bottom-right, bottom, bottom-left, left, top-left) and the four
/// paddings, the Fluent window shadow's look (a soft dark falloff, a little deeper below).
unsafe fn kde_shadow(x: &X, scale: f32) {
    let (tiles, pads) = super::linux::shadow_tiles(scale);
    // A 32-bit TrueColor visual for the images' masks (any compositing server has one).
    let mut vi: xlib::XVisualInfo = std::mem::zeroed();
    if (x.xl.XMatchVisualInfo)(x.dpy, (x.xl.XDefaultScreen)(x.dpy), 32, xlib::TrueColor, &mut vi) == 0 {
        return;
    }
    let mut ids: Vec<c_long> = Vec::with_capacity(12);
    for (w, h, mut px) in tiles {
        let (w, h) = (&w, &h);
        let pm = (x.xl.XCreatePixmap)(x.dpy, x.root, *w as c_uint, *h as c_uint, 32);
        let gc = (x.xl.XCreateGC)(x.dpy, pm, 0, std::ptr::null_mut());
        // A depth-32 image over the default visual's masks (ARGB8888 little-endian).
        let img = (x.xl.XCreateImage)(
            x.dpy,
            vi.visual,
            32,
            xlib::ZPixmap,
            0,
            px.as_mut_ptr() as *mut c_char,
            *w as c_uint,
            *h as c_uint,
            32,
            0,
        );
        if !img.is_null() {
            (x.xl.XPutImage)(x.dpy, pm, gc, img, 0, 0, 0, 0, *w as c_uint, *h as c_uint);
            destroy_borrowed_image(img);
        }
        (x.xl.XFreeGC)(x.dpy, gc);
        ids.push(pm as c_long);
    }
    ids.extend(pads.map(|p| p as c_long));
    let atom = X::atom(&x.xl, x.dpy, "_KDE_NET_WM_SHADOW");
    (x.xl.XChangeProperty)(
        x.dpy,
        x.win,
        atom,
        xlib::XA_CARDINAL,
        32,
        xlib::PropModeReplace,
        ids.as_ptr() as *const c_uchar,
        ids.len() as c_int,
    );
}

fn cursor_index(c: Cursor) -> usize {
    match c {
        Cursor::Arrow => 0,
        Cursor::Hand => 1,
        Cursor::Text => 2,
        Cursor::Grab | Cursor::Grabbing => 3,
        Cursor::ScrollNS => 4,
    }
}

/// X's buttons 1, 2, 3.
const X_BUTTONS: [MouseButton; 3] = [MouseButton::Left, MouseButton::Middle, MouseButton::Right];

/// The X11 window's part of the turn (`runner::Host`): its connection, backend and what its
/// events leave between turns.
struct XHost {
    x: X,
    be: Box<dyn Backend>,
    backend: super::BackendKind,
    t0: Instant,
    /// The wake pipe (read end, write end) and whether a wake is unread.
    fds: [c_int; 2],
    pending: Arc<AtomicBool>,
    /// The window's size in pixels as last configured.
    size: (i32, i32),
    cursor: Cursor,
    /// A text field has the focus (`ui.ime_on()`), and the window has it: the input method's
    /// context has the focus while both do.
    ime_on: bool,
    focused: bool,
    pressed_in_client: bool,
    caption: CaptionPress,
    edge: Option<c_long>,
    sync_value: Option<x11_dl::sync::XSyncValue>,
    /// Frames are paced by the swap chain; turns and tickers (an animation's steps) by the
    /// refresh rate of the monitor under the window (RandR), looked up again when the window has
    /// moved.
    randr: Option<x11_dl::xrandr::Xrandr>,
    placed_at: Option<f64>,
    /// The window's accessibility tree (AT-SPI), when the app asked for it.
    access: Option<crate::access::Bridge>,
    /// The window is an overlay (`OverlayOptions`).
    overlay: Option<overlay::Overlay>,
}

impl runner::Host for XHost {
    fn access(&mut self) -> Option<&mut crate::access::Bridge> {
        self.access.as_mut()
    }

    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn events(&mut self, ui: &mut Ui, app: &mut dyn App, pacer: &mut Pacer) {
        // SAFETY: the module contract.
        unsafe {
            let mut buf = [0u8; 64];
            if libc::read(self.fds[0], buf.as_mut_ptr() as *mut _, buf.len()) > 0 {
                super::woken(&self.pending, ui);
            }
            let mut resized = None;
            let mut bounds_changed = false;
            while (self.x.xl.XPending)(self.x.dpy) > 0 {
                let mut ev: xlib::XEvent = std::mem::zeroed();
                (self.x.xl.XNextEvent)(self.x.dpy, &mut ev);
                if ev.get_type() == xlib::GenericEvent {
                    if let Some(o) = &mut self.overlay {
                        if let Some(i) = o.raw_motion(&self.x, &mut ev, self.size) {
                            ui.input(i);
                        }
                    }
                    continue;
                }
                let filtered = (self.x.xl.XFilterEvent)(&mut ev, 0) != 0;
                // The input method's callbacks ran inside Xlib: their preedit, in order with
                // the keys and commits around it.
                let (xl, dpy, win) = (&self.x.xl, self.x.dpy, self.x.win);
                self.x.xim.serve(xl, dpy, win, |i| ui.input(i));
                if filtered {
                    continue;
                }
                match ev.get_type() {
                    xlib::ClientMessage => {
                        let c = ev.client_message;
                        if c.message_type == self.x.atoms.wm_protocols {
                            let p = c.data.get_long(0) as c_ulong;
                            if p == self.x.atoms.wm_delete {
                                ui.window.req.close = true;
                            } else if p == self.x.atoms.sync_request {
                                // Answered after the frame at the new size (or now, if none).
                                self.sync_value = Some(x11_dl::sync::XSyncValue {
                                    lo: c.data.get_long(2) as c_uint,
                                    hi: c.data.get_long(3) as c_int,
                                });
                            }
                        }
                    }
                    xlib::ConfigureNotify => {
                        let c = ev.configure;
                        self.placed_at.get_or_insert(self.now());
                        bounds_changed = true;
                        if c.width > 0 && c.height > 0 {
                            resized = Some((c.width, c.height));
                            if let Some(o) = &mut self.overlay {
                                if let Some(p) = o.configured(&self.x, (c.width, c.height)) {
                                    ui.window.place = Some(p);
                                    ui.events.push(crate::Event::Moved);
                                }
                            }
                        }
                    }
                    xlib::Expose => ui.tree.expose(),
                    xlib::PropertyNotify if ev.property.atom == self.x.atoms.net_wm_state => {
                        super::set_maximized(ui, self.x.is_maximized());
                        bounds_changed = true;
                    }
                    xlib::MotionNotify => {
                        let m = ev.motion;
                        // A caption press that left the slop is a move: the app's press ends
                        // here, and the window manager takes the pointer.
                        if m.state & xlib::Button1Mask != 0 {
                            if let Some((px, py)) = self.caption.moved(m.x, m.y) {
                                pointer_taken(ui, &mut self.pressed_in_client, (px, py), MouseButton::Left);
                                self.x.moveresize(px, py, 8);
                                continue;
                            }
                        }
                        // Over the resize border (not while a button is held in the client).
                        let e = if self.pressed_in_client { None } else { border_dir(ui, m.x, m.y) };
                        if e != self.edge {
                            self.edge = e;
                            let c = match e {
                                Some(d) => self.x.edge_cursors[d as usize],
                                None => self.x.cursors[cursor_index(self.cursor)],
                            };
                            (self.x.xl.XDefineCursor)(self.x.dpy, self.x.win, c);
                        }
                        ui.input(Input::Move(m.x, m.y));
                    }
                    xlib::LeaveNotify => {
                        self.edge = None;
                        if ev.crossing.mode == xlib::NotifyNormal {
                            ui.input(Input::Leave);
                        }
                    }
                    xlib::ButtonPress => {
                        let b = ev.button;
                        match b.button {
                            4 | 5 => ui.input(Input::Wheel(b.x, b.y, if b.button == 4 { 1.0 } else { -1.0 })),
                            1..=3 => {
                                let button = X_BUTTONS[b.button as usize - 1];
                                let hit = app.nc_hit(ui, b.x, b.y);
                                // The edges belong to the window manager.
                                if button == MouseButton::Left && hit != NcHit::ClientControl {
                                    if let Some(d) = border_dir(ui, b.x, b.y) {
                                        self.x.moveresize(b.x, b.y, d);
                                        continue;
                                    }
                                }
                                // A press on the caption is the app's first (`CaptionPress`).
                                if button == MouseButton::Left && hit == NcHit::Caption {
                                    self.caption.press(b.x, b.y, &mut ui.window);
                                }
                                self.pressed_in_client = true;
                                ui.input(Input::Mods(mods(b.state)));
                                ui.input(Input::Down(b.x, b.y, button));
                                // The window menu takes the pointer: the release is the window
                                // manager's.
                                if button == MouseButton::Right
                                    && hit == NcHit::Caption
                                    && self.x.show_window_menu(b.x, b.y)
                                {
                                    pointer_taken(ui, &mut self.pressed_in_client, (b.x, b.y), MouseButton::Right);
                                }
                            }
                            8 => ui.input(Input::Key(crate::input::key::BROWSER_BACK, Mods::default())),
                            _ => {}
                        }
                    }
                    xlib::ButtonRelease => {
                        let b = ev.button;
                        if b.button == 1 {
                            self.caption.release();
                        }
                        if (1..=3).contains(&b.button) && self.pressed_in_client {
                            ui.input(Input::Up(b.x, b.y, X_BUTTONS[b.button as usize - 1]));
                            let held = b.state & (xlib::Button1Mask | xlib::Button2Mask | xlib::Button3Mask);
                            let this = [xlib::Button1Mask, xlib::Button2Mask, xlib::Button3Mask][b.button as usize - 1];
                            self.pressed_in_client = held & !this != 0;
                        }
                    }
                    xlib::KeyPress => {
                        let mut k = ev.key;
                        let sym = (self.x.xl.XLookupKeysym)(&mut k, 0);
                        if let Some(code) = vk(sym) {
                            ui.input(Input::Key(code, mods(k.state)));
                        }
                        // Text: through the input method when there is one (a commit comes as a
                        // key press of its own).
                        let mut text = vec![0u8; 64];
                        let mut keysym = 0;
                        let mut status = 0;
                        let ic = self.x.xim.ic();
                        let n = if !ic.is_null() {
                            let mut n = 0;
                            for _ in 0..2 {
                                n = (self.x.xl.Xutf8LookupString)(
                                    ic,
                                    &mut k,
                                    text.as_mut_ptr() as *mut c_char,
                                    text.len() as c_int,
                                    &mut keysym,
                                    &mut status,
                                );
                                // XBufferOverflow: a long commit; `n` is the size it needs.
                                if status != -1 || n <= 0 {
                                    break;
                                }
                                text = vec![0u8; n as usize];
                            }
                            n
                        } else {
                            (self.x.xl.XLookupString)(
                                &mut k,
                                text.as_mut_ptr() as *mut c_char,
                                text.len() as c_int,
                                &mut keysym,
                                std::ptr::null_mut(),
                            )
                        };
                        if n > 0 && k.state & xlib::ControlMask == 0 {
                            let n = (n as usize).min(text.len());
                            let s = String::from_utf8_lossy(&text[..n]).into_owned();
                            if s.chars().count() > 1 {
                                ui.input(Input::Commit(s));
                            } else {
                                for c in s.chars() {
                                    ui.input(Input::Char(c));
                                }
                            }
                        }
                    }
                    xlib::FocusIn => {
                        self.focused = true;
                        self.x.xim.focus(&self.x.xl, self.ime_on, false);
                        ui.input(Input::Focus(true));
                    }
                    xlib::FocusOut => {
                        self.focused = false;
                        self.x.xim.focus(&self.x.xl, false, false);
                        ui.input(Input::Focus(false));
                    }
                    xlib::SelectionRequest => self.x.selection_request(&ev.selection_request),
                    xlib::SelectionClear => self.x.clipboard = None,
                    _ => {}
                }
            }
            // An ordinary window's bounds, for the app to keep (`Window::bounds`).
            if bounds_changed && self.overlay.is_none() {
                let bounds = self.x.bounds(ui.window.bounds);
                if bounds != ui.window.bounds {
                    ui.window.bounds = bounds;
                    ui.events.push(crate::Event::Moved);
                }
            }
            // Moved or resized: maybe onto another monitor (once the window has been still for
            // half a second, not on every step of a drag).
            if self.placed_at.is_some_and(|t| self.now() - t > 0.5) {
                self.placed_at = None;
                pacer.set_hz(refresh_hz(&self.x, self.randr.as_ref(), self.size.0, self.size.1));
            }
            if let Some((w, h)) = resized {
                if (w, h) != self.size {
                    self.size = (w, h);
                    let fit = crate::render::vulkan::surface_fit(self.x.pres_size, (w, h));
                    if fit != self.x.pres_size {
                        self.x.pres_size = fit;
                        (self.x.xl.XResizeWindow)(self.x.dpy, self.x.pres, fit.0 as c_uint, fit.1 as c_uint);
                        // The surface's new extent must be known before the swap chain is rebuilt.
                        (self.x.xl.XSync)(self.x.dpy, 0);
                    }
                    let scale = ui.tree.scale();
                    ui.resize(w, h, scale);
                }
            }
        }
    }

    fn requests(&mut self, ui: &mut Ui, req: super::Requests) {
        let x = &self.x;
        // SAFETY: the module contract.
        unsafe {
            if req.minimize {
                (x.xl.XIconifyWindow)(x.dpy, x.win, (x.xl.XDefaultScreen)(x.dpy));
            }
            if req.toggle_maximize {
                x.wm_state(!x.is_maximized(), x.atoms.max_h, x.atoms.max_v);
            }
            if let Some(f) = req.fullscreen {
                if f != ui.window.fullscreen {
                    x.wm_state(f, x.atoms.fullscreen, 0);
                }
                ui.window.fullscreen = f;
            }
            if let Some(t) = req.title {
                if let Some(a) = &mut self.access {
                    a.title = t.clone();
                }
                set_title(&x.xl, x.dpy, x.win, &x.atoms, t);
            }
            if let Some(o) = &mut self.overlay {
                if let Some((w, h)) = req.size {
                    let k = ui.tree.scale();
                    let size = ((w * k).round() as i32, (h * k).round() as i32);
                    if size != self.size {
                        o.set_size(x, size.0, size.1);
                    }
                }
                if let Some(on) = req.click_through {
                    o.set_click_through(x, on);
                }
            }
        }
    }

    fn cursor_and_ime(&mut self, ui: &Ui) {
        let x = &self.x;
        // SAFETY: the module contract.
        unsafe {
            if ui.cursor != self.cursor {
                self.cursor = ui.cursor;
                if self.edge.is_none() {
                    (x.xl.XDefineCursor)(x.dpy, x.win, x.cursors[cursor_index(self.cursor)]);
                }
            }
        }
        // The input method composes only into a text field: elsewhere its context has no focus,
        // so keys reach the app's shortcuts (Space, arrows). A field that lost the focus drops
        // the composition in progress.
        let x = &mut self.x;
        // SAFETY: the module contract.
        unsafe {
            if self.ime_on != ui.ime_on() {
                self.ime_on = ui.ime_on();
                x.xim.focus(&x.xl, self.ime_on && self.focused, !self.ime_on);
            }
            // Candidates under the caret: the spot is the line's bottom.
            if let Some((cx, cy, h)) = ui.io.ime_at.filter(|_| self.ime_on) {
                x.xim.set_spot(&x.xl, cx, cy + h);
            }
        }
    }

    fn clipboard_out(&mut self, text: String) {
        // SAFETY: the module contract.
        unsafe { self.x.set_clipboard(text) }
    }

    fn clipboard_in(&mut self) -> Option<String> {
        // SAFETY: the module contract.
        unsafe { self.x.paste() }
    }

    fn picks(&mut self, ui: &mut Ui) {
        super::linux::serve_picks(ui);
    }

    /// X11 has no system edge for an undecorated window.
    fn edge(&self, ui: &Ui) -> Option<crate::geom::Color> {
        ui.window.framed().then_some(super::FRAME_EDGE)
    }

    fn backend(&mut self) -> &mut dyn Backend {
        self.be.as_mut()
    }

    fn renew_backend(&mut self) -> bool {
        let (x, size, backend) = (&self.x, self.size, self.backend);
        if self.overlay.is_some() {
            return runner::replace_backend(&mut self.be, || transparent_backend(x, size.0, size.1).ok());
        }
        runner::replace_backend(&mut self.be, || Some(make_backend(backend, x, size.0, size.1)))
    }

    fn turn_done(&mut self) {
        let x = &self.x;
        // SAFETY: the module contract.
        unsafe {
            // Tell the WM this size is drawn: it sends the next one only now.
            if let (Some(v), Some((xe, c))) = (self.sync_value.take(), x.sync.as_ref()) {
                (xe.XSyncSetCounter)(x.dpy, *c, v);
            }
            (x.xl.XFlush)(x.dpy);
        }
    }
}

/// Opens the window and runs `app` until the window closes.
pub(super) fn run(opts: Options, make: impl FnMut(&mut Ui) -> Box<dyn App>) -> Result<(), crate::Error> {
    // SAFETY: the module contract.
    let (x, _) = unsafe { X::open(&opts, None) }.map_err(crate::Error::Platform)?;
    let access = opts.accessibility.then_some(opts.title);
    host_loop(x, opts.backend, access, None, make)
}

/// Opens an overlay (`OverlayOptions`) and runs `app` in it until it closes. `Err` when the
/// display has no transparent window to give (no 32-bit visual, no Vulkan surface composing
/// alpha).
pub(super) fn run_overlay(
    opts: OverlayOptions,
    make: impl FnOnce(&mut Ui) -> Box<dyn App>,
) -> Result<(), crate::Error> {
    let main = Options {
        title: opts.title.clone(),
        width: opts.width.round().max(1.0) as i32,
        height: opts.height.round().max(1.0) as i32,
        min_width: 1,
        min_height: 1,
        backend: opts.backend,
        accessibility: false,
        bounds: None,
    };
    // SAFETY: the module contract.
    let (x, place) = unsafe { X::open(&main, Some(&opts)) }.map_err(crate::Error::Platform)?;
    let mut o = overlay::Overlay::new(place.expect("an overlay is opened at a place"));
    if opts.click_through {
        // SAFETY: the module contract.
        unsafe { o.set_click_through(&x, true) };
    }
    host_loop(x, opts.backend, None, Some(o), make)
}

/// Runs the app in a window opened (`run`, `run_overlay`) until it closes: `access`, the title
/// its accessibility tree is named by (none: no tree).
fn host_loop(
    x: X,
    backend: super::BackendKind,
    access: Option<String>,
    overlay: Option<overlay::Overlay>,
    make: impl FnOnce(&mut Ui) -> Box<dyn App>,
) -> Result<(), crate::Error> {
    // SAFETY: the module contract.
    unsafe {
        let mut fds = [0 as c_int; 2];
        // Unchecked, a failed pipe left `fds` at 0: the flags and the final close hit stdin.
        super::linux::wake_pipe(&mut fds).map_err(|e| crate::Error::Platform(format!("wake pipe: {e}")))?;
        let pending = Arc::new(AtomicBool::new(false));
        let waker = HostWake { fd: fds[1], pending: pending.clone() };
        let scale = dpi_scale(&x.xl, x.dpy);
        let (cw, ch) = x.size();
        let be = match overlay {
            Some(_) => match transparent_backend(&x, cw, ch) {
                Ok(be) => be,
                Err(e) => {
                    libc::close(fds[0]);
                    libc::close(fds[1]);
                    return Err(crate::Error::Platform(format!("an overlay needs a transparent window: {e}")));
                }
            },
            None => make_backend(backend, &x, cw, ch),
        };
        let mut ui = Ui::new(cw, ch, scale);
        ui.window.handle = x.win as isize;
        if overlay.is_none() {
            ui.window.bounds = x.bounds(None);
        }
        if let Some(o) = &overlay {
            ui.window.place = Some(o.place);
            let work = overlay::work_area(&x);
            ui.window.work_area = Some((work.w as f32 / scale, work.h as f32 / scale));
        }
        let task_waker = waker.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || task_waker.wake());
        ui.set_task_wake(wake.clone());
        let mut app = make(&mut ui);
        let randr = x11_dl::xrandr::Xrandr::open().ok();
        let is_overlay = overlay.is_some();
        let mut host = XHost {
            x,
            be,
            backend,
            t0: Instant::now(),
            fds,
            pending,
            size: (cw, ch),
            cursor: Cursor::Arrow,
            ime_on: false,
            focused: false,
            pressed_in_client: false,
            caption: CaptionPress::default(),
            edge: None,
            sync_value: None,
            randr,
            placed_at: None,
            access: access.map(|title| super::linux::access(wake.clone(), title)),
            overlay,
        };
        runner::start(&mut host, &mut ui, app.as_mut());
        if !is_overlay {
            kde_shadow(&host.x, scale);
        }
        (host.x.xl.XMapWindow)(host.x.dpy, host.x.win);
        (host.x.xl.XFlush)(host.x.dpy);

        let xfd = (host.x.xl.XConnectionNumber)(host.x.dpy);
        let mut pacer = Pacer::new(refresh_hz(&host.x, host.randr.as_ref(), cw, ch));
        // Work since the heap was last trimmed: trimmed once each time the loop goes idle.
        let mut untrimmed = super::linux::IdleTrim::default();
        loop {
            // Block until X input, a wake, or the next scheduled frame.
            if (host.x.xl.XPending)(host.x.dpy) == 0 {
                let timeout = pacer.timeout(&ui, runner::Host::now(&host), true, false);
                untrimmed.before_wait(timeout);
                if timeout != Some(0.0) {
                    let mut p = [
                        libc::pollfd { fd: xfd, events: libc::POLLIN, revents: 0 },
                        libc::pollfd { fd: fds[0], events: libc::POLLIN, revents: 0 },
                    ];
                    // To the microsecond (`runner`): poll's whole milliseconds miss a refresh.
                    let ts = runner::timespec(timeout);
                    libc::ppoll(
                        p.as_mut_ptr(),
                        2,
                        ts.as_ref().map_or(std::ptr::null(), |t| t as *const _),
                        std::ptr::null(),
                    );
                }
            }
            if !runner::turn(&mut host, &mut ui, app.as_mut(), &mut pacer) {
                break;
            }
        }
        drop(app);
        let XHost { x, be, .. } = host;
        drop(be);
        let X { xl, dpy, win, xim, .. } = x;
        xim.close(&xl, dpy);
        (xl.XDestroyWindow)(dpy, win);
        (xl.XCloseDisplay)(dpy);
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
    Ok(())
}

/// The refresh rate of the monitor (RandR CRTC) under the window's centre; 60 Hz without RandR
/// or when it reports none.
fn refresh_hz(x: &X, randr: Option<&x11_dl::xrandr::Xrandr>, w: i32, h: i32) -> f64 {
    let Some(rr) = randr else { return 60.0 };
    // SAFETY: the display and windows are live; every structure RandR returns is read while it
    // lives and freed once with its own free function.
    unsafe {
        let (mut cx, mut cy, mut child) = (0, 0, 0);
        (x.xl.XTranslateCoordinates)(x.dpy, x.win, x.root, w / 2, h / 2, &mut cx, &mut cy, &mut child);
        let res = (rr.XRRGetScreenResourcesCurrent)(x.dpy, x.root);
        if res.is_null() {
            return 60.0;
        }
        let r = &*res;
        let modes = std::slice::from_raw_parts(r.modes, r.nmode.max(0) as usize);
        let mut hz = None;
        for &crtc in std::slice::from_raw_parts(r.crtcs, r.ncrtc.max(0) as usize) {
            let info = (rr.XRRGetCrtcInfo)(x.dpy, res, crtc);
            if info.is_null() {
                continue;
            }
            let c = &*info;
            let inside = cx >= c.x && cy >= c.y && cx < c.x + c.width as i32 && cy < c.y + c.height as i32;
            if c.mode != 0 && inside {
                hz = modes
                    .iter()
                    .find(|m| m.id == c.mode && m.hTotal > 0 && m.vTotal > 0)
                    .map(|m| m.dotClock as f64 / (f64::from(m.hTotal) * f64::from(m.vTotal)));
            }
            (rr.XRRFreeCrtcInfo)(info);
            if hz.is_some() {
                break;
            }
        }
        (rr.XRRFreeScreenResources)(res);
        super::refresh_or_60(hz)
    }
}

/// Destroys an XImage made over pixels it does not own: its data is detached first, so that
/// XDestroyImage does not free them.
///
/// # Safety
/// `img` is a live XImage of ours, not used after.
unsafe fn destroy_borrowed_image(img: *mut xlib::XImage) {
    // SAFETY: as the caller promises.
    unsafe {
        (*img).data = std::ptr::null_mut();
        if let Some(destroy) = (*img).funcs.destroy_image {
            destroy(img);
        }
    }
}

/// The window's title, as both WM_NAME and EWMH's UTF-8 `_NET_WM_NAME`.
///
/// # Safety
/// `dpy` is open and `win` a window of it; `xl` and `atoms` are this display's.
unsafe fn set_title(xl: &xlib::Xlib, dpy: *mut xlib::Display, win: c_ulong, atoms: &Atoms, title: String) {
    let c = CString::new(title).unwrap_or_default();
    // SAFETY: as the caller promises; `c` outlives both calls, which copy it.
    unsafe {
        (xl.XStoreName)(dpy, win, c.as_ptr());
        (xl.XChangeProperty)(
            dpy,
            win,
            atoms.net_wm_name,
            atoms.utf8,
            8,
            xlib::PropModeReplace,
            c.as_ptr() as *const c_uchar,
            c.as_bytes().len() as c_int,
        );
    }
}
