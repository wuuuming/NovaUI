//! X11 input methods (XIM): fcitx5 and IBus through their XIM servers. Over-the-spot where the
//! server offers it: the server draws the preedit and its candidates at the spot (the caret).
//! On-the-spot (the preedit through callbacks, drawn in the field: `Input::Preedit`) only where
//! the server offers nothing else: Xlib sends no spot for it, so a server places its candidates
//! at the window's corner (fcitx5 does), and fcitx5 draws its preedit itself unless told
//! otherwise. Committed text arrives as key presses
//! (`Xutf8LookupString`, the host's). The context has the focus only while a text field has it,
//! so keys reach the app's shortcuts everywhere else. A server that starts after the window, or
//! restarts, is taken up (`XRegisterIMInstantiateCallback`, the input method's destroy callback).
//!
//! The contract every `unsafe` block here relies on, beyond `x11.rs`'s: the callbacks' client
//! data is `Xim::cb`, a heap block that lives from `Xim::new` until `Xim::close`, after the
//! context, the input method and the instantiate callback are gone. Xlib calls the callbacks
//! only from inside Xlib calls on the window's thread, and no reference to the block is held
//! across such a call.

use super::linux::Preedit;
use crate::input::Input;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long, c_short, c_ulong, c_ushort, c_void};
use x11_dl::xlib;

/// What the callbacks leave for the host: the preedit and what the server did.
#[derive(Default)]
struct Shared {
    /// The preedit and its caret, in chars.
    text: Vec<char>,
    caret: usize,
    /// The preedit changed since the host last took it.
    changed: bool,
    /// An input method server appeared (instantiate callback) / the open one went away
    /// (destroy callback: its context and itself are gone without being closed).
    appeared: bool,
    gone: bool,
}

impl Shared {
    /// The preedit is empty: no text, the caret at 0.
    fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// A preedit draw: `len` chars from `first` replaced by `new`, the caret at `caret`.
    fn draw(&mut self, caret: i32, first: i32, len: i32, new: &[char]) {
        let n = self.text.len();
        let a = usize::try_from(first).unwrap_or(0).min(n);
        let b = a.saturating_add(usize::try_from(len).unwrap_or(0)).min(n);
        self.text.splice(a..b, new.iter().copied());
        self.caret = usize::try_from(caret).unwrap_or(0).min(self.text.len());
        self.changed = true;
    }

    /// A caret move: where it lands. Word and line moves the field has no measure of keep it.
    fn move_caret(&mut self, dir: xlib::XIMCaretDirection, position: i32) -> usize {
        use xlib::XIMCaretDirection as D;
        let to = match dir {
            D::XIMForwardChar => self.caret + 1,
            D::XIMBackwardChar => self.caret.saturating_sub(1),
            D::XIMLineStart => 0,
            D::XIMLineEnd => self.text.len(),
            D::XIMAbsolutePosition => usize::try_from(position).unwrap_or(0),
            _ => self.caret,
        }
        .min(self.text.len());
        if to != self.caret {
            self.caret = to;
            self.changed = true;
        }
        to
    }

    /// The preedit ended.
    fn done(&mut self) {
        if !self.text.is_empty() {
            self.clear();
            self.changed = true;
        }
    }
}

/// `XIMStyles` (Xlib.h; not in x11-dl).
#[repr(C)]
struct XimStyles {
    count: c_ushort,
    styles: *mut c_ulong,
}

const PREEDIT_CALLBACKS: c_ulong = xlib::XIMPreeditCallbacks as c_ulong;
const PREEDIT_POSITION: c_ulong = xlib::XIMPreeditPosition as c_ulong;
const PREEDIT_NOTHING: c_ulong = xlib::XIMPreeditNothing as c_ulong;
const PREEDIT_NONE: c_ulong = xlib::XIMPreeditNone as c_ulong;
const STATUS_NOTHING: c_ulong = xlib::XIMStatusNothing as c_ulong;
const STATUS_NONE: c_ulong = xlib::XIMStatusNone as c_ulong;

/// The styles asked for, best first: over-the-spot, on-the-spot, then committed text only.
const STYLES: [c_ulong; 6] = [
    PREEDIT_POSITION | STATUS_NOTHING,
    PREEDIT_POSITION | STATUS_NONE,
    PREEDIT_CALLBACKS | STATUS_NOTHING,
    PREEDIT_CALLBACKS | STATUS_NONE,
    PREEDIT_NOTHING | STATUS_NOTHING,
    PREEDIT_NONE | STATUS_NONE,
];

/// The window's input method and context.
pub(super) struct Xim {
    im: xlib::XIM,
    ic: xlib::XIC,
    style: c_ulong,
    cb: *mut Shared,
    /// The window's own event mask: the context's filter events are added to it.
    mask: c_long,
    /// The instantiate callback is registered (no server yet, or it went away).
    waiting: bool,
    /// The context should have the focus (a text field has it, in the focused window).
    focused: bool,
    /// The caret, where the candidates (over-the-spot: the preedit) stand: window pixels.
    spot: Option<(i32, i32)>,
    shown: Preedit,
    /// Over-the-spot's font set: Xlib makes no such context without one, though the servers draw
    /// with their own fonts. Made with the first context; null when the display has no fonts.
    fonts: xlib::XFontSet,
}

impl Xim {
    /// The window's input method: opened now, or when a server appears.
    ///
    /// # Safety
    /// `x11.rs`'s contract: `xl` loaded, `dpy` and `win` live until `close`.
    pub(super) unsafe fn new(xl: &xlib::Xlib, dpy: *mut xlib::Display, win: c_ulong, mask: c_long) -> Xim {
        let mut x = Xim {
            im: std::ptr::null_mut(),
            ic: std::ptr::null_mut(),
            style: 0,
            cb: Box::into_raw(Box::default()),
            mask,
            waiting: false,
            focused: false,
            spot: None,
            shown: Preedit::default(),
            fonts: std::ptr::null_mut(),
        };
        if !x.open(xl, dpy, win) {
            x.wait(xl, dpy);
        }
        x
    }

    /// No input method (an overlay, which never has the keyboard): keys are looked up plainly.
    pub(super) fn none(mask: c_long) -> Xim {
        Xim {
            im: std::ptr::null_mut(),
            ic: std::ptr::null_mut(),
            style: 0,
            cb: Box::into_raw(Box::default()),
            mask,
            waiting: false,
            focused: false,
            spot: None,
            shown: Preedit::default(),
            fonts: std::ptr::null_mut(),
        }
    }

    /// The context, for `Xutf8LookupString`; null without one.
    pub(super) fn ic(&self) -> xlib::XIC {
        self.ic
    }

    /// Opens the server's input method and a context in the best style it takes.
    unsafe fn open(&mut self, xl: &xlib::Xlib, dpy: *mut xlib::Display, win: c_ulong) -> bool {
        let im = (xl.XOpenIM)(dpy, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        if im.is_null() {
            return false;
        }
        let mut styles: *mut XimStyles = std::ptr::null_mut();
        let failed = (xl.XGetIMValues)(
            im,
            xlib::XNQueryInputStyle_0.as_ptr(),
            &mut styles as *mut *mut XimStyles,
            std::ptr::null_mut::<c_char>(),
        );
        let offered: Vec<c_ulong> = if failed.is_null() && !styles.is_null() {
            let s = &*styles;
            let v = if s.styles.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(s.styles, usize::from(s.count)).to_vec()
            };
            (xl.XFree)(styles as *mut c_void);
            v
        } else {
            // Not said: each style is tried in turn.
            STYLES.to_vec()
        };
        let destroy = xlib::XIMCallback { client_data: self.cb as xlib::XPointer, callback: Some(im_destroyed) };
        (xl.XSetIMValues)(
            im,
            xlib::XNDestroyCallback_0.as_ptr(),
            &destroy as *const xlib::XIMCallback,
            std::ptr::null_mut::<c_char>(),
        );
        if offered.iter().any(|s| s & PREEDIT_POSITION != 0) {
            self.font_set(xl, dpy);
        }
        for style in STYLES.into_iter().filter(|s| offered.contains(s)) {
            let ic = self.create_ic(xl, im, win, style);
            if !ic.is_null() {
                self.im = im;
                self.ic = ic;
                self.style = style;
                // The events the context needs (key releases for some servers) reach the window.
                let mut filter: c_ulong = 0;
                (xl.XGetICValues)(
                    ic,
                    xlib::XNFilterEvents_0.as_ptr(),
                    &mut filter as *mut c_ulong,
                    std::ptr::null_mut::<c_char>(),
                );
                (xl.XSelectInput)(dpy, win, self.mask | filter as c_long);
                if self.focused {
                    (xl.XSetICFocus)(ic);
                }
                if let Some((x, y)) = self.spot.take() {
                    self.set_spot(xl, x, y);
                }
                return true;
            }
        }
        (xl.XCloseIM)(im);
        false
    }

    /// The font set over-the-spot names (`fonts`): any font, in every charset the server has.
    unsafe fn font_set(&mut self, xl: &xlib::Xlib, dpy: *mut xlib::Display) -> xlib::XFontSet {
        if self.fonts.is_null() {
            let (mut missing, mut count, mut def) = (std::ptr::null_mut(), 0, std::ptr::null_mut());
            self.fonts = (xl.XCreateFontSet)(
                dpy,
                c"-*-*-*-*-*-*-*-*-*-*-*-*-*-*,*".as_ptr(),
                &mut missing,
                &mut count,
                &mut def,
            );
            if !missing.is_null() {
                (xl.XFreeStringList)(missing);
            }
        }
        self.fonts
    }

    unsafe fn create_ic(&self, xl: &xlib::Xlib, im: xlib::XIM, win: c_ulong, style: c_ulong) -> xlib::XIC {
        let end = std::ptr::null_mut::<c_char>();
        let cb = |f: unsafe extern "C" fn(xlib::XIC, xlib::XPointer, xlib::XPointer) -> c_int| xlib::XICCallback {
            client_data: self.cb as xlib::XPointer,
            callback: Some(f),
        };
        let (start, done, draw, caret) = (cb(preedit_start), cb(preedit_done), cb(preedit_draw), cb(preedit_caret));
        let spot = xlib::XPoint { x: 0, y: 0 };
        let attrs = match style & 0xff {
            PREEDIT_CALLBACKS => (xl.XVaCreateNestedList)(
                0,
                xlib::XNPreeditStartCallback_0.as_ptr(),
                &start as *const xlib::XICCallback,
                xlib::XNPreeditDoneCallback_0.as_ptr(),
                &done as *const xlib::XICCallback,
                xlib::XNPreeditDrawCallback_0.as_ptr(),
                &draw as *const xlib::XICCallback,
                xlib::XNPreeditCaretCallback_0.as_ptr(),
                &caret as *const xlib::XICCallback,
                end,
            ),
            PREEDIT_POSITION if !self.fonts.is_null() => (xl.XVaCreateNestedList)(
                0,
                xlib::XNSpotLocation_0.as_ptr(),
                &spot as *const xlib::XPoint,
                xlib::XNFontSet_0.as_ptr(),
                self.fonts,
                end,
            ),
            // Without a font set Xlib makes no over-the-spot context: the next style is taken.
            PREEDIT_POSITION => return std::ptr::null_mut(),
            _ => std::ptr::null_mut(),
        };
        let ic = if attrs.is_null() {
            (xl.XCreateIC)(
                im,
                xlib::XNInputStyle_0.as_ptr(),
                style,
                xlib::XNClientWindow_0.as_ptr(),
                win,
                xlib::XNFocusWindow_0.as_ptr(),
                win,
                end,
            )
        } else {
            let ic = (xl.XCreateIC)(
                im,
                xlib::XNInputStyle_0.as_ptr(),
                style,
                xlib::XNClientWindow_0.as_ptr(),
                win,
                xlib::XNFocusWindow_0.as_ptr(),
                win,
                xlib::XNPreeditAttributes_0.as_ptr(),
                attrs,
                end,
            );
            (xl.XFree)(attrs);
            ic
        };
        // Created without the focus: it follows the text field (`focus`).
        if !ic.is_null() {
            (xl.XUnsetICFocus)(ic);
        }
        ic
    }

    /// No server: wait for one.
    unsafe fn wait(&mut self, xl: &xlib::Xlib, dpy: *mut xlib::Display) {
        if !self.waiting {
            self.waiting = (xl.XRegisterIMInstantiateCallback)(
                dpy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                Some(im_instantiated),
                self.cb as *mut c_char,
            ) != 0;
        }
    }

    unsafe fn unwait(&mut self, xl: &xlib::Xlib, dpy: *mut xlib::Display) {
        if std::mem::take(&mut self.waiting) {
            (xl.XUnregisterIMInstantiateCallback)(
                dpy,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                Some(im_instantiated),
                self.cb as *mut c_char,
            );
        }
    }

    /// Gives the context the focus or takes it. `reset`: the composition in progress is dropped
    /// (the field lost the focus), not kept for when the window has it again.
    pub(super) unsafe fn focus(&mut self, xl: &xlib::Xlib, on: bool, reset: bool) {
        if self.focused == on {
            return;
        }
        self.focused = on;
        if self.ic.is_null() {
            return;
        }
        if on {
            (xl.XSetICFocus)(self.ic);
        } else {
            (xl.XUnsetICFocus)(self.ic);
            if reset {
                let left = (xl.Xutf8ResetIC)(self.ic);
                if !left.is_null() {
                    (xl.XFree)(left as *mut c_void);
                }
                // The field dropped its preedit with the focus.
                let s = &mut *self.cb;
                s.clear();
                s.changed = false;
                self.shown.ended();
            }
        }
    }

    /// The caret (window pixels, at the line's bottom): the server's candidates stand there.
    pub(super) unsafe fn set_spot(&mut self, xl: &xlib::Xlib, x: i32, y: i32) {
        if self.spot == Some((x, y)) {
            return;
        }
        self.spot = Some((x, y));
        // Only over-the-spot has a spot: Xlib refuses it to the other styles.
        if self.ic.is_null() || self.style & PREEDIT_POSITION == 0 {
            return;
        }
        let clamp = |v: i32| v.clamp(i32::from(c_short::MIN), i32::from(c_short::MAX)) as c_short;
        let spot = xlib::XPoint { x: clamp(x), y: clamp(y) };
        let end = std::ptr::null_mut::<c_char>();
        let attrs = (xl.XVaCreateNestedList)(0, xlib::XNSpotLocation_0.as_ptr(), &spot as *const xlib::XPoint, end);
        if !attrs.is_null() {
            (xl.XSetICValues)(self.ic, xlib::XNPreeditAttributes_0.as_ptr(), attrs, end);
            (xl.XFree)(attrs);
        }
    }

    /// After each event Xlib handled: what the callbacks left, as `Ui` input, and a server that
    /// came or went.
    pub(super) unsafe fn serve(
        &mut self,
        xl: &xlib::Xlib,
        dpy: *mut xlib::Display,
        win: c_ulong,
        mut input: impl FnMut(Input),
    ) {
        let (appeared, gone) = {
            let s = &mut *self.cb;
            (std::mem::take(&mut s.appeared), std::mem::take(&mut s.gone))
        };
        if gone {
            // The server went: its context and input method are gone with it.
            self.im = std::ptr::null_mut();
            self.ic = std::ptr::null_mut();
            self.style = 0;
            (*self.cb).done();
            self.wait(xl, dpy);
        }
        if appeared && self.im.is_null() {
            self.unwait(xl, dpy);
            if !self.open(xl, dpy, win) {
                self.wait(xl, dpy);
            }
        }
        let s = &mut *self.cb;
        if std::mem::take(&mut s.changed) {
            self.shown.set(s.text.iter().collect(), s.caret, &mut input);
        }
    }

    /// The context and input method go, then the callbacks' block.
    pub(super) unsafe fn close(mut self, xl: &xlib::Xlib, dpy: *mut xlib::Display) {
        self.unwait(xl, dpy);
        if !self.ic.is_null() {
            (xl.XDestroyIC)(self.ic);
        }
        if !self.im.is_null() {
            (xl.XCloseIM)(self.im);
        }
        if !self.fonts.is_null() {
            (xl.XFreeFontSet)(dpy, self.fonts);
        }
        drop(Box::from_raw(self.cb));
    }
}

/// `XIMText` as chars: wide chars, or multibyte text in the locale's encoding.
unsafe fn xim_text(t: &xlib::XIMText) -> Vec<char> {
    let p = t.string.multi_byte;
    let n = usize::from(t.length);
    if p.is_null() || n == 0 {
        return Vec::new();
    }
    if t.encoding_is_wchar != 0 {
        let w = std::slice::from_raw_parts(p as *const libc::wchar_t, n);
        return w.iter().filter_map(|&c| char::from_u32(c as u32)).collect();
    }
    let mut w: Vec<libc::wchar_t> = vec![0; n + 1];
    let got = mbstowcs(w.as_mut_ptr(), p, n);
    if got == usize::MAX {
        // Not the locale's encoding: most servers send UTF-8.
        return CStr::from_ptr(p).to_string_lossy().chars().take(n).collect();
    }
    w[..got].iter().filter_map(|&c| char::from_u32(c as u32)).collect()
}

extern "C" {
    /// The C library's multibyte (the locale's encoding) to wide-char conversion; not in `libc`.
    fn mbstowcs(dest: *mut libc::wchar_t, src: *const c_char, n: usize) -> usize;
}

unsafe extern "C" fn im_instantiated(_: *mut xlib::Display, client: xlib::XPointer, _: xlib::XPointer) {
    (*(client as *mut Shared)).appeared = true;
}

unsafe extern "C" fn im_destroyed(_: xlib::XIM, client: xlib::XPointer, _: xlib::XPointer) {
    (*(client as *mut Shared)).gone = true;
}

/// The preedit's length is not limited (−1).
unsafe extern "C" fn preedit_start(_: xlib::XIC, _: xlib::XPointer, _: xlib::XPointer) -> c_int {
    -1
}

unsafe extern "C" fn preedit_done(_: xlib::XIC, client: xlib::XPointer, _: xlib::XPointer) -> c_int {
    (*(client as *mut Shared)).done();
    0
}

unsafe extern "C" fn preedit_draw(_: xlib::XIC, client: xlib::XPointer, call: xlib::XPointer) -> c_int {
    let s = &mut *(client as *mut Shared);
    let d = &*(call as *const xlib::XIMPreeditDrawCallbackStruct);
    let new = if d.text.is_null() { Vec::new() } else { xim_text(&*d.text) };
    s.draw(d.caret, d.chg_first, d.chg_length, &new);
    0
}

unsafe extern "C" fn preedit_caret(_: xlib::XIC, client: xlib::XPointer, call: xlib::XPointer) -> c_int {
    let s = &mut *(client as *mut Shared);
    let c = &mut *(call as *mut xlib::XIMPreeditCaretCallbackStruct);
    // The callback answers with where the caret went.
    c.position = s.move_caret(c.direction, c.position) as c_int;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn preedit_draws_replace_spans_and_place_the_caret() {
        let mut s = Shared::default();
        s.draw(5, 0, 0, &chars("zhong"));
        assert_eq!((s.text.iter().collect::<String>(), s.caret), ("zhong".into(), 5));
        // The server replaces the whole span with the converted text.
        s.draw(2, 0, 5, &chars("中国"));
        assert_eq!((s.text.iter().collect::<String>(), s.caret), ("中国".into(), 2));
        // A deletion (no text) past the end is clamped.
        s.draw(9, 1, 9, &[]);
        assert_eq!((s.text.iter().collect::<String>(), s.caret), ("中".into(), 1));
        assert_eq!(s.move_caret(xlib::XIMCaretDirection::XIMBackwardChar, 0), 0);
        assert_eq!(s.move_caret(xlib::XIMCaretDirection::XIMLineEnd, 0), 1);
        assert_eq!(s.move_caret(xlib::XIMCaretDirection::XIMAbsolutePosition, 7), 1);
        s.done();
        assert!(s.text.is_empty() && s.changed);
    }
}
