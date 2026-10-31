//! Wayland host (libwayland-client, loaded at run time). One xdg-shell toplevel without server
//! decorations: the app draws its title bar; moving and resizing are the compositor's
//! (`xdg_toplevel.move` / `.resize` on a press). An overlay is a `zwlr_layer_shell_v1` surface
//! instead (the overlay layer, anchored at the bottom, placed by its margins, moved by the host
//! as the caption is dragged); a compositor without the layer shell gets it through XWayland. Fractional scale comes from
//! `wp_fractional_scale_v1` (with `wp_viewporter` mapping the buffer onto the logical size), else
//! the outputs' integer scale. Keys go through xkbcommon into Windows virtual-key codes and text.
//! The loop blocks in `poll` on the Wayland socket and the wake pipe until input, a wake or the
//! next scheduled frame. Presentation is Vulkan's FIFO swap chain on the `wl_surface`.
//!
//! The window manager does not draw a shadow for a client-decorated window: KWin takes one from
//! `org_kde_kwin_shadow` (the same tiles as X11's `_KDE_NET_WM_SHADOW`).
//!
//! Input methods (fcitx5, IBus) come through `zwp_text_input_v3`: enabled while a text field has
//! the focus, its preedit drawn in the field at the caret, its candidates placed at the cursor
//! rectangle (`ime_at`), its commits inserted; elsewhere it is off and keys reach the app.
//!
//! xkbcommon is called through the function table of the library loaded for the whole process;
//! `Keys` owns its context, keymap and state (each null or live, unreferenced once), so a call
//! on one of them through `&self` / `&mut self` is sound. libc calls take live locals and
//! descriptors this module opened and has not closed.

use super::linux::{border_dir, pointer_taken, vk as vk_code, CaptionPress, HostWake, Preedit};
use super::FRAME_EDGE;
use super::{App, Options, OverlayOptions, Place};
use crate::app::{NcHit, Ui};
use crate::input::{Input, Mods, MouseButton};
use crate::render::primitive::Backend;
use crate::runner::{self, Pacer};
use crate::style::Cursor;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::raw::{c_char, c_int};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_keyboard,
    wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1};
use wayland_protocols::wp::text_input::zv3::client::{zwp_text_input_manager_v3, zwp_text_input_v3};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::decoration::zv1::client::{zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_plasma::shadow::client::{org_kde_kwin_shadow, org_kde_kwin_shadow_manager};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
use xkbcommon_dl as xkb;

/// Linux evdev button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const URI_LIST: &str = "text/uri-list";
const TEXT_UTF8: &str = "text/plain;charset=utf-8";

/// True when this process should open a Wayland window: a Wayland session (`WAYLAND_DISPLAY`)
/// and no `NOVAUI_X11=1` override. XWayland stays available through the X11 host.
pub fn wanted() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        && std::env::var_os("NOVAUI_X11").is_none_or(|v| v.is_empty() || v == "0")
}

/// Keyboard state: the compositor's keymap through xkbcommon.
struct Keys {
    x: &'static xkb::XkbCommon,
    ctx: *mut xkb::xkb_context,
    map: *mut xkb::xkb_keymap,
    state: *mut xkb::xkb_state,
    /// Repeat: keys per second, delay in ms (wl_keyboard.repeat_info).
    rate: i32,
    delay: i32,
}

impl Keys {
    fn new() -> Option<Keys> {
        let x = xkb::xkbcommon_option()?;
        // SAFETY: a plain flag; the context is owned by `Keys` and unreferenced in `Drop`.
        let ctx = unsafe { (x.xkb_context_new)(xkb::xkb_context_flags::XKB_CONTEXT_NO_FLAGS) };
        (!ctx.is_null()).then_some(Keys {
            x,
            ctx,
            map: std::ptr::null_mut(),
            state: std::ptr::null_mut(),
            rate: 25,
            delay: 400,
        })
    }

    /// # Safety
    /// `fd` is the compositor's keymap: a file of at least `size` bytes holding a NUL-terminated
    /// XKB keymap string, as `wl_keyboard.keymap` promises.
    unsafe fn set_keymap(&mut self, fd: OwnedFd, size: u32) {
        let p = libc::mmap(std::ptr::null_mut(), size as usize, libc::PROT_READ, libc::MAP_PRIVATE, fd.as_raw_fd(), 0);
        if p == libc::MAP_FAILED {
            return;
        }
        let map = (self.x.xkb_keymap_new_from_string)(
            self.ctx,
            p as *const c_char,
            xkb::xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
            xkb::xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
        );
        libc::munmap(p, size as usize);
        if map.is_null() {
            return;
        }
        let state = (self.x.xkb_state_new)(map);
        if state.is_null() {
            (self.x.xkb_keymap_unref)(map);
            return;
        }
        self.drop_map();
        self.map = map;
        self.state = state;
    }

    /// # Safety
    /// None beyond the module's: the keymap and state are null or live, and nulled here.
    unsafe fn drop_map(&mut self) {
        if !self.state.is_null() {
            (self.x.xkb_state_unref)(self.state);
        }
        if !self.map.is_null() {
            (self.x.xkb_keymap_unref)(self.map);
        }
        self.state = std::ptr::null_mut();
        self.map = std::ptr::null_mut();
    }

    fn mods(&self) -> Mods {
        if self.state.is_null() {
            return Mods::default();
        }
        // SAFETY: the state is live (checked above); each name is a NUL-terminated literal.
        let on = |n: &[u8]| unsafe {
            (self.x.xkb_state_mod_name_is_active)(
                self.state,
                n.as_ptr() as *const c_char,
                xkb::xkb_state_component::XKB_STATE_MODS_EFFECTIVE,
            ) > 0
        };
        Mods { shift: on(xkb::XKB_MOD_NAME_SHIFT), ctrl: on(xkb::XKB_MOD_NAME_CTRL), alt: on(xkb::XKB_MOD_NAME_ALT) }
    }

    /// (virtual key, text) for an evdev key code.
    fn key(&self, code: u32) -> (Option<u32>, String) {
        if self.state.is_null() {
            return (None, String::new());
        }
        let kc = code + 8;
        // SAFETY: the state is live (checked above); xkb writes at most `buf.len()` bytes to `buf`.
        unsafe {
            let sym = (self.x.xkb_state_key_get_one_sym)(self.state, kc);
            let mut buf = [0u8; 64];
            let n = (self.x.xkb_state_key_get_utf8)(self.state, kc, buf.as_mut_ptr() as *mut c_char, buf.len());
            let text = if n > 0 { String::from_utf8_lossy(&buf[..n as usize]).into_owned() } else { String::new() };
            (vk_code(sym as _), text)
        }
    }

    fn repeats(&self, code: u32) -> bool {
        // SAFETY: the keymap is live (checked first).
        !self.map.is_null() && unsafe { (self.x.xkb_keymap_key_repeats)(self.map, code + 8) } != 0
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        // SAFETY: the keymap and state go first; the context, live since `new`, once.
        unsafe {
            self.drop_map();
            (self.x.xkb_context_unref)(self.ctx);
        }
    }
}

/// An xdg_toplevel configure: the size the compositor chose (if any), maximized, fullscreen.
type Configure = (Option<(i32, i32)>, bool, bool);

/// What the event handlers hand the loop.
#[derive(Default)]
struct Pending {
    /// xdg configure: (logical size if the compositor chose one, maximized, fullscreen, serial).
    configure: Option<Configure>,
    configure_serial: Option<u32>,
    close: bool,
    input: Vec<Input>,
    /// Logical pointer position.
    pointer: Option<(f64, f64)>,
    /// Pressed with the left button at (logical) — the loop decides move/resize/client.
    left_down: Option<(f64, f64, u32)>,
    left_up: Option<(f64, f64)>,
    /// Pressed with the right button at (logical): on the caption, the window menu.
    right_down: Option<(f64, f64, u32)>,
    pointer_enter_serial: Option<u32>,
    pointer_left: bool,
    /// Key down (evdev code) for repeat; None = released.
    repeat: Option<Option<u32>>,
    /// Fractional scale, 120ths.
    frac: Option<u32>,
    /// A layer surface's configure: its serial and the size the compositor gives (0: ours).
    layer_configure: Option<(u32, u32, u32)>,
    files: Option<Vec<PathBuf>>,
    paste: Option<String>,
}

/// The text input's state (`zwp_text_input_v3`): what the compositor said and what was last sent.
#[derive(Default)]
struct TextIn {
    /// The text input is on our surface (it follows the keyboard focus).
    entered: bool,
    /// Sent: enabled, and at which cursor rectangle (logical). An `enter` clears both: the
    /// compositor ignores what came before it, so the state is sent again.
    enabled: bool,
    rect: Option<(i32, i32, i32, i32)>,
    /// Double-buffered until `done`: the preedit with its caret (chars), the committed text.
    preedit: Option<(String, usize)>,
    commit: Option<String>,
    shown: Preedit,
}

impl TextIn {
    /// The compositor forgot what was sent (a new enter, a leave): enabled and the caret's rect
    /// are sent again.
    fn forget_sent(&mut self) {
        self.enabled = false;
        self.rect = None;
    }

    /// `done`: what came since the last one, as `Ui` input. A preedit not repeated before a
    /// `done` is gone (the protocol resets it to empty).
    fn done(&mut self, out: &mut Vec<Input>) {
        let mut push = |i| out.push(i);
        if let Some(c) = self.commit.take().filter(|c| !c.is_empty()) {
            push(Input::Commit(c));
            self.shown.ended();
        }
        let (t, c) = self.preedit.take().unwrap_or_default();
        self.shown.set(t, c, &mut push);
    }
}

/// A preedit's caret, given in bytes of `text` (−1: hidden), as chars; a hidden caret or one off
/// a char boundary stands at the end.
fn preedit_caret(text: &str, begin: i32) -> usize {
    match usize::try_from(begin).ok().and_then(|b| text.get(..b)) {
        Some(head) => head.chars().count(),
        None => text.chars().count(),
    }
}

struct Out {
    output: wl_output::WlOutput,
    scale: i32,
    /// The current mode's refresh rate in mHz; 0 until the output says.
    refresh: i32,
    /// The current mode's size in pixels, and the output's logical size (xdg-output) when the
    /// compositor says.
    mode: (i32, i32),
    logical: Option<(i32, i32)>,
    xdg: Option<zxdg_output_v1::ZxdgOutputV1>,
}

impl Out {
    /// Its size in surface coordinates: the logical size, else the mode over the scale.
    fn logical_size(&self) -> Option<(i32, i32)> {
        self.logical.or_else(|| {
            let s = self.scale.max(1);
            (self.mode.0 > 0).then_some((self.mode.0 / s, self.mode.1 / s))
        })
    }
}

struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    wm: Option<xdg_wm_base::XdgWmBase>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<wl_seat::WlSeat>,
    deco: Option<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1>,
    frac_mgr: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    shadow_mgr: Option<org_kde_kwin_shadow_manager::OrgKdeKwinShadowManager>,
    ddm: Option<wl_data_device_manager::WlDataDeviceManager>,
    ti_mgr: Option<zwp_text_input_manager_v3::ZwpTextInputManagerV3>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    xdg_outputs: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    text_input: Option<zwp_text_input_v3::ZwpTextInputV3>,
    ti: TextIn,
    outputs: Vec<Out>,
    /// Outputs the surface is on (for the integer-scale fallback).
    on: Vec<wl_output::WlOutput>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keys: Option<Keys>,
    p: Pending,
    /// The latest input serial (pointer button or key), for move/resize/clipboard.
    serial: u32,
    /// Drag-and-drop / clipboard offers.
    dnd_offer: Option<(wl_data_offer::WlDataOffer, Vec<String>)>,
    sel_offer: Option<(wl_data_offer::WlDataOffer, Vec<String>)>,
    offers: Vec<(wl_data_offer::WlDataOffer, Vec<String>)>,
    dnd_serial: u32,
    /// Text we own on the clipboard.
    clip: Option<String>,
    conn: Connection,
}

impl State {
    /// The outputs the surface is on; every output before it is on one (at start, and on a
    /// compositor that has not said yet).
    fn outputs_on(&self) -> impl Iterator<Item = &Out> {
        self.outputs.iter().filter(|o| self.on.is_empty() || self.on.contains(&o.output))
    }

    /// The refresh rate of the fastest output the surface is on (of any output before it is
    /// on one); 60 Hz when no output says. Turns and tickers (an animation's steps) are paced
    /// by it; frames by the swap chain.
    /// The logical size of the output the surface is on (of the first, before it is on one).
    fn output_size(&self) -> Option<(i32, i32)> {
        self.outputs_on().find_map(Out::logical_size)
    }

    /// Asks each output for its logical size (xdg-output), once the manager is bound.
    fn ask_output_sizes(&mut self, qh: &QueueHandle<Self>) {
        let Some(m) = self.xdg_outputs.clone() else { return };
        for o in self.outputs.iter_mut().filter(|o| o.xdg.is_none()) {
            o.xdg = Some(m.get_xdg_output(&o.output, qh, o.output.clone()));
        }
    }

    fn refresh_hz(&self) -> f64 {
        let mhz = self.outputs_on().map(|o| o.refresh).max();
        super::refresh_or_60(mhz.map(|mhz| f64::from(mhz) / 1000.0))
    }

    fn offer_mimes(&mut self, o: &wl_data_offer::WlDataOffer) -> Vec<String> {
        self.offers.iter().position(|(x, _)| x == o).map(|i| self.offers.remove(i).1).unwrap_or_default()
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        r: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = e else { return };
        match interface.as_str() {
            "wl_compositor" => s.compositor = Some(r.bind(name, version.min(5), qh, ())),
            "xdg_wm_base" => s.wm = Some(r.bind(name, version.min(4), qh, ())),
            "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
            "wl_seat" => s.seat = Some(r.bind(name, version.min(7), qh, ())),
            "zxdg_decoration_manager_v1" => s.deco = Some(r.bind(name, 1, qh, ())),
            "wp_fractional_scale_manager_v1" => s.frac_mgr = Some(r.bind(name, 1, qh, ())),
            "wp_viewporter" => s.viewporter = Some(r.bind(name, 1, qh, ())),
            "org_kde_kwin_shadow_manager" => s.shadow_mgr = Some(r.bind(name, version.min(2), qh, ())),
            "wl_data_device_manager" => s.ddm = Some(r.bind(name, version.min(3), qh, ())),
            "zwp_text_input_manager_v3" => s.ti_mgr = Some(r.bind(name, 1, qh, ())),
            "zwlr_layer_shell_v1" => s.layer_shell = Some(r.bind(name, version.min(4), qh, ())),
            "zxdg_output_manager_v1" => s.xdg_outputs = Some(r.bind(name, version.min(3), qh, ())),
            "wl_output" => {
                let o: wl_output::WlOutput = r.bind(name, version.min(2), qh, ());
                s.outputs.push(Out { output: o, scale: 1, refresh: 0, mode: (0, 0), logical: None, xdg: None });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        s: &mut Self,
        o: &wl_output::WlOutput,
        e: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = s.outputs.iter_mut().find(|x| &x.output == o) else { return };
        match e {
            wl_output::Event::Scale { factor } => out.scale = factor.max(1),
            wl_output::Event::Mode { flags, refresh, width, height }
                if flags.into_result().is_ok_and(|f| f.contains(wl_output::Mode::Current)) =>
            {
                out.refresh = refresh;
                out.mode = (width, height);
            }
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, wl_output::WlOutput> for State {
    fn event(
        s: &mut Self,
        _: &zxdg_output_v1::ZxdgOutputV1,
        e: zxdg_output_v1::Event,
        output: &wl_output::WlOutput,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_output_v1::Event::LogicalSize { width, height } = e {
            if let Some(o) = s.outputs.iter_mut().find(|o| &o.output == output) {
                o.logical = Some((width, height));
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        e: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                s.p.layer_configure = Some((serial, width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => s.p.close = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_surface::WlSurface,
        e: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_surface::Event::Enter { output } => s.on.push(output),
            wl_surface::Event::Leave { output } => s.on.retain(|o| o != &output),
            _ => {}
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        s: &mut Self,
        _: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            s.p.configure_serial = Some(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        s: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        e: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            xdg_toplevel::Event::Configure { width, height, states } => {
                let st: Vec<u32> =
                    states.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect();
                let has = |v: xdg_toplevel::State| st.contains(&(v as u32));
                let size = (width > 0 && height > 0).then_some((width, height));
                s.p.configure = Some((size, has(xdg_toplevel::State::Maximized), has(xdg_toplevel::State::Fullscreen)));
            }
            xdg_toplevel::Event::Close => s.p.close = true,
            _ => {}
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        e: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = e {
            s.p.frac = Some(scale);
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(s: &mut Self, seat: &wl_seat::WlSeat, e: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = e {
            if c.contains(wl_seat::Capability::Pointer) && s.pointer.is_none() {
                s.pointer = Some(seat.get_pointer(qh, ()));
            }
            if c.contains(wl_seat::Capability::Keyboard) && s.keyboard.is_none() {
                s.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter { serial, surface_x, surface_y, .. } => {
                s.p.pointer_enter_serial = Some(serial);
                s.p.pointer = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Leave { .. } => s.p.pointer_left = true,
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => s.p.pointer = Some((surface_x, surface_y)),
            wl_pointer::Event::Button { serial, button, state, .. } => {
                s.serial = serial;
                let down = state == WEnum::Value(wl_pointer::ButtonState::Pressed);
                let (x, y) = s.p.pointer.unwrap_or_default();
                match button {
                    BTN_LEFT if down => s.p.left_down = Some((x, y, serial)),
                    BTN_LEFT => s.p.left_up = Some((x, y)),
                    BTN_RIGHT | BTN_MIDDLE => {
                        let b = if button == BTN_RIGHT { MouseButton::Right } else { MouseButton::Middle };
                        if b == MouseButton::Right && down {
                            s.p.right_down = Some((x, y, serial));
                        }
                        // Logical → physical is the loop's; stash with a marker.
                        if down {
                            s.p.input.push(Input::Mods(s.keys.as_ref().map_or_else(Mods::default, Keys::mods)));
                        }
                        s.p.input.push(if down {
                            Input::Down(x as i32, y as i32, b)
                        } else {
                            Input::Up(x as i32, y as i32, b)
                        });
                    }
                    BTN_SIDE if down => s.p.input.push(Input::Key(crate::input::key::BROWSER_BACK, Mods::default())),
                    _ => {}
                }
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } => {
                let (x, y) = s.p.pointer.unwrap_or_default();
                // 10 logical units per wheel notch on most compositors.
                s.p.input.push(Input::Wheel(x as i32, y as i32, (-value / 10.0) as f32));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        s: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                if let Some(k) = s.keys.as_mut() {
                    // SAFETY: an xkb_v1 keymap event: `fd` and `size` are as the protocol promises.
                    unsafe { k.set_keymap(fd, size) };
                }
            }
            wl_keyboard::Event::Enter { serial, .. } => {
                s.serial = serial;
                s.p.input.push(Input::Focus(true));
            }
            wl_keyboard::Event::Leave { .. } => {
                s.p.repeat = Some(None);
                s.p.input.push(Input::Focus(false));
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(k) = s.keys.as_ref().filter(|k| !k.state.is_null()) {
                    // SAFETY: the state is live (filtered above); plain values.
                    unsafe {
                        (k.x.xkb_state_update_mask)(k.state, mods_depressed, mods_latched, mods_locked, 0, 0, group)
                    };
                }
            }
            wl_keyboard::Event::Key { serial, key, state, .. } => {
                s.serial = serial;
                let Some(k) = s.keys.as_ref() else { return };
                if state == WEnum::Value(wl_keyboard::KeyState::Pressed) {
                    key_input(k, key, &mut s.p.input);
                    s.p.repeat = Some(k.repeats(key).then_some(key));
                } else {
                    s.p.repeat = Some(None);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                if let Some(k) = s.keys.as_mut() {
                    k.rate = rate;
                    k.delay = delay;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()> for State {
    fn event(
        s: &mut Self,
        _: &zwp_text_input_v3::ZwpTextInputV3,
        e: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_text_input_v3::Event as E;
        let ti = &mut s.ti;
        match e {
            E::Enter { .. } => {
                ti.entered = true;
                ti.forget_sent();
            }
            E::Leave { .. } => {
                ti.entered = false;
                ti.forget_sent();
                ti.preedit = None;
                ti.commit = None;
                ti.shown.set(String::new(), 0, &mut |i| s.p.input.push(i));
            }
            E::PreeditString { text, cursor_begin, .. } => {
                let t = text.unwrap_or_default();
                let c = preedit_caret(&t, cursor_begin);
                ti.preedit = Some((t, c));
            }
            E::CommitString { text } => ti.commit = text,
            // No surrounding text is sent, so there is none to delete.
            E::Done { .. } => ti.done(&mut s.p.input),
            _ => {}
        }
    }
}

/// One key press as `Ui` input: the key, then its text (not with Ctrl: shortcuts are keys).
fn key_input(k: &Keys, code: u32, out: &mut Vec<Input>) {
    let mods = k.mods();
    let (v, text) = k.key(code);
    if let Some(v) = v {
        out.push(Input::Key(v, mods));
    }
    if !mods.ctrl {
        for c in text.chars().filter(|c| !c.is_control()) {
            out.push(Input::Char(c));
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for State {
    // `data_offer` creates the offer object: without this the first clipboard or drag offer the
    // compositor sends (at once, when the clipboard is not empty) aborts the process.
    wayland_client::event_created_child!(State, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, ()),
    ]);

    fn event(
        s: &mut Self,
        _: &wl_data_device::WlDataDevice,
        e: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_data_device::Event::DataOffer { id } => s.offers.push((id, Vec::new())),
            wl_data_device::Event::Enter { serial, id, .. } => {
                s.dnd_serial = serial;
                s.dnd_offer = id.map(|o| {
                    let m = s.offer_mimes(&o);
                    let accepts = m.iter().any(|t| t == URI_LIST);
                    o.accept(serial, accepts.then(|| URI_LIST.to_string()));
                    if o.version() >= 3 {
                        o.set_actions(wl_data_device_manager::DndAction::Copy, wl_data_device_manager::DndAction::Copy);
                    }
                    (o, m)
                });
            }
            wl_data_device::Event::Leave => {
                if let Some((o, _)) = s.dnd_offer.take() {
                    o.destroy();
                }
            }
            wl_data_device::Event::Drop => {
                if let Some((o, m)) = s.dnd_offer.take() {
                    if m.iter().any(|t| t == URI_LIST) {
                        let bytes = receive(&s.conn, &o, URI_LIST);
                        s.p.files = Some(uri_paths(&String::from_utf8_lossy(&bytes)));
                        if o.version() >= 3 {
                            o.finish();
                        }
                    }
                    o.destroy();
                }
            }
            wl_data_device::Event::Selection { id } => {
                if let Some((o, _)) = s.sel_offer.take() {
                    o.destroy();
                }
                s.sel_offer = id.map(|o| {
                    let m = s.offer_mimes(&o);
                    (o, m)
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for State {
    fn event(
        s: &mut Self,
        o: &wl_data_offer::WlDataOffer,
        e: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = e {
            if let Some((_, m)) = s.offers.iter_mut().find(|(x, _)| x == o) {
                m.push(mime_type);
            }
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for State {
    fn event(
        s: &mut Self,
        src: &wl_data_source::WlDataSource,
        e: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_data_source::Event::Send { fd, .. } => {
                if let Some(t) = &s.clip {
                    let mut f = std::fs::File::from(fd);
                    let _ = std::io::Write::write_all(&mut f, t.as_bytes());
                }
            }
            wl_data_source::Event::Cancelled => {
                s.clip = None;
                src.destroy();
            }
            _ => {}
        }
    }
}

/// Reads what an offer holds as `mime` (blocking, briefly: the source writes and closes).
fn receive(conn: &Connection, o: &wl_data_offer::WlDataOffer, mime: &str) -> Vec<u8> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` is a live local of the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Vec::new();
    }
    // SAFETY: the two descriptors pipe2 just opened, each owned once from here.
    let (r, w) = unsafe {
        use std::os::fd::FromRawFd;
        (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
    };
    o.receive(mime.to_string(), w.as_fd());
    drop(w);
    let _ = conn.flush();
    let mut out = Vec::new();
    let mut f = std::fs::File::from(r);
    // A source in this same process answers on our queue: bounded wait, then give up.
    // SAFETY: `f`'s own open descriptor; flags only.
    unsafe {
        let fd = f.as_raw_fd();
        libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
    }
    let t0 = Instant::now();
    let mut buf = [0u8; 8192];
    while t0.elapsed() < std::time::Duration::from_millis(500) {
        match std::io::Read::read(&mut f, &mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let mut p = libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 };
                // SAFETY: `p` is a live local pollfd of `f`'s open descriptor.
                unsafe { libc::poll(&mut p, 1, 50) };
            }
            Err(_) => break,
        }
    }
    out
}

/// `text/uri-list` → local paths (percent-decoded `file://` lines).
fn uri_paths(list: &str) -> Vec<PathBuf> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        .map(|p| p.find('/').map_or(p, |i| &p[i..]))
        .map(|p| {
            let b = p.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() {
                    if let Ok(v) = u8::from_str_radix(&p[i + 1..i + 3], 16) {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                }
                out.push(b[i]);
                i += 1;
            }
            use std::os::unix::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_vec(out))
        })
        .collect()
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore zxdg_decoration_manager_v1::ZxdgDecorationManagerV1);
delegate_noop!(State: ignore zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1);
delegate_noop!(State: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);
delegate_noop!(State: ignore wp_viewporter::WpViewporter);
delegate_noop!(State: ignore wp_viewport::WpViewport);
delegate_noop!(State: ignore org_kde_kwin_shadow_manager::OrgKdeKwinShadowManager);
delegate_noop!(State: ignore org_kde_kwin_shadow::OrgKdeKwinShadow);
delegate_noop!(State: ignore wl_data_device_manager::WlDataDeviceManager);
delegate_noop!(State: ignore zwp_text_input_manager_v3::ZwpTextInputManagerV3);
delegate_noop!(State: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(State: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);
delegate_noop!(State: ignore wl_region::WlRegion);

/// A shared-memory buffer (shadow tiles, cursor images): ARGB8888 premultiplied.
fn shm_buffer(
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<State>,
    w: i32,
    h: i32,
    bgra: &[u8],
) -> Option<wl_buffer::WlBuffer> {
    let len = (w * h * 4) as usize;
    let name = c"novaui-shm";
    // SAFETY: `name` is a NUL-terminated literal.
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: the descriptor memfd_create just opened, owned once from here.
    let fd = unsafe {
        use std::os::fd::FromRawFd;
        OwnedFd::from_raw_fd(fd)
    };
    let mut f = std::fs::File::from(fd);
    std::io::Write::write_all(&mut f, &bgra[..len]).ok()?;
    let pool = shm.create_pool(f.as_fd(), len as i32, qh, ());
    let b = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    Some(b)
}

/// The surface's role: a window's (an xdg toplevel) or an overlay's (a layer surface).
enum Role {
    Toplevel { xdg: xdg_surface::XdgSurface, top: xdg_toplevel::XdgToplevel },
    Layer(zwlr_layer_surface_v1::ZwlrLayerSurfaceV1),
}

/// The window: its surface, roles and the scale it is drawn at.
struct Win {
    surface: wl_surface::WlSurface,
    role: Role,
    viewport: Option<wp_viewport::WpViewport>,
    _frac: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    /// Logical size (surface coordinates).
    lw: i32,
    lh: i32,
    /// Scale: 120ths with fractional scaling, else the integer buffer scale ×120.
    scale120: u32,
    shadow: Option<org_kde_kwin_shadow::OrgKdeKwinShadow>,
    shadow_scale: u32,
}

impl Win {
    /// The toplevel, when the window is one (not an overlay).
    fn top(&self) -> Option<&xdg_toplevel::XdgToplevel> {
        match &self.role {
            Role::Toplevel { top, .. } => Some(top),
            Role::Layer(_) => None,
        }
    }

    fn scale(&self) -> f32 {
        self.scale120 as f32 / 120.0
    }
    /// The whole scale buffers and cursors are made at (at least 1): the scale rounded down.
    fn buffer_scale(&self) -> u32 {
        (self.scale120 / 120).max(1)
    }
    /// A new scale (1/120ths): recorded; `true` when it moved.
    fn set_scale120(&mut self, s: u32) -> bool {
        let moved = s != self.scale120;
        self.scale120 = s;
        moved
    }
    /// Physical (buffer) size.
    fn px(&self) -> (i32, i32) {
        let s = self.scale();
        (((self.lw as f32) * s).round() as i32, ((self.lh as f32) * s).round() as i32)
    }
    /// Tell the compositor how the buffer maps onto the logical size.
    fn apply_scale(&self) {
        if let Some(v) = &self.viewport {
            // The swap chain is larger than the window in 256-px steps (`vk::surface_fit`, so a
            // live resize does not rebuild it every frame) and the canvas sits in its top-left:
            // show only that part. Without the source rectangle the whole buffer is mapped onto
            // the window, and the stale rows and columns past the canvas (frames of earlier
            // sizes) show at its right and bottom edges.
            let (pw, ph) = self.px();
            v.set_source(0.0, 0.0, pw as f64, ph as f64);
            v.set_destination(self.lw, self.lh);
        } else {
            self.surface.set_buffer_scale(self.buffer_scale() as i32);
        }
    }
}

/// Server-drawn cursors (the theme's images), by name.
struct Cursors {
    theme: Option<wayland_cursor::CursorTheme>,
    surface: wl_surface::WlSurface,
    shown: Option<&'static str>,
}

fn cursor_name(c: Cursor) -> &'static str {
    match c {
        Cursor::Arrow => "default",
        Cursor::Hand => "pointer",
        Cursor::Text => "text",
        Cursor::Grab => "grab",
        Cursor::Grabbing => "grabbing",
        Cursor::ScrollNS => "ns-resize",
    }
}

fn edge_cursor(d: xdg_toplevel::ResizeEdge) -> &'static str {
    use xdg_toplevel::ResizeEdge as E;
    match d {
        E::TopLeft => "nw-resize",
        E::Top => "n-resize",
        E::TopRight => "ne-resize",
        E::Right => "e-resize",
        E::BottomRight => "se-resize",
        E::Bottom => "s-resize",
        E::BottomLeft => "sw-resize",
        E::Left => "w-resize",
        _ => "default",
    }
}

/// `edge_dir`'s `_NET_WM_MOVERESIZE` numbering → xdg's resize edge.
fn resize_edge(d: i64) -> xdg_toplevel::ResizeEdge {
    use xdg_toplevel::ResizeEdge as E;
    match d {
        0 => E::TopLeft,
        1 => E::Top,
        2 => E::TopRight,
        3 => E::Right,
        4 => E::BottomRight,
        5 => E::Bottom,
        6 => E::BottomLeft,
        _ => E::Left,
    }
}

impl Cursors {
    fn set(&mut self, pointer: &wl_pointer::WlPointer, serial: u32, name: &'static str, scale: i32) {
        if self.shown == Some(name) {
            return;
        }
        let Some(theme) = self.theme.as_mut() else { return };
        let fallback = |n: &str| match n {
            "pointer" => "hand2",
            "text" => "xterm",
            "grab" | "grabbing" => "fleur",
            "default" => "left_ptr",
            "nw-resize" => "top_left_corner",
            "ne-resize" => "top_right_corner",
            "se-resize" => "bottom_right_corner",
            "sw-resize" => "bottom_left_corner",
            "n-resize" => "top_side",
            "s-resize" => "bottom_side",
            "e-resize" => "right_side",
            "w-resize" => "left_side",
            _ => "left_ptr",
        };
        let c = match theme.get_cursor(name) {
            Some(c) => c,
            None => match theme.get_cursor(fallback(name)) {
                Some(c) => c,
                None => return,
            },
        };
        let img = &c[0];
        let (hx, hy) = img.hotspot();
        let buf: &wl_buffer::WlBuffer = img;
        self.surface.set_buffer_scale(scale);
        self.surface.attach(Some(buf), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.surface.commit();
        pointer.set_cursor(serial, Some(&self.surface), hx as i32 / scale, hy as i32 / scale);
        self.shown = Some(name);
    }
}

/// Lets a shadow go. `destroy` exists from version 2; a version 1 shadow (an older KWin) cannot
/// be destroyed, and sending it is a protocol error that ends the connection. Its replacement,
/// made for the same surface, takes its place at the next commit.
fn drop_shadow(sh: org_kde_kwin_shadow::OrgKdeKwinShadow) {
    if sh.version() >= 2 {
        sh.destroy();
    }
}

/// Sets KWin's shadow around the window (tiles at the window's scale).
fn kde_shadow(st: &State, qh: &QueueHandle<State>, win: &mut Win) {
    let (Some(mgr), Some(shm)) = (st.shadow_mgr.as_ref(), st.shm.as_ref()) else { return };
    if win.shadow.is_some() && win.shadow_scale == win.scale120 {
        return;
    }
    if let Some(old) = win.shadow.take() {
        drop_shadow(old);
    }
    // Tiles are in buffer pixels at the integer scale ≥ the window's; offsets are logical.
    let s = win.scale().ceil().max(1.0);
    let (tiles, pads) = super::linux::shadow_tiles(s);
    let sh = mgr.create(&win.surface, qh, ());
    let mut bufs = Vec::new();
    for (w, h, px) in &tiles {
        bufs.push(shm_buffer(shm, qh, *w, *h, px));
    }
    // Order: top, top-right, right, bottom-right, bottom, bottom-left, left, top-left.
    let at = |i: usize| bufs[i].as_ref();
    if let (Some(t), Some(tr), Some(r), Some(br), Some(b), Some(bl), Some(l), Some(tl)) =
        (at(0), at(1), at(2), at(3), at(4), at(5), at(6), at(7))
    {
        sh.attach_top(t);
        sh.attach_top_right(tr);
        sh.attach_right(r);
        sh.attach_bottom_right(br);
        sh.attach_bottom(b);
        sh.attach_bottom_left(bl);
        sh.attach_left(l);
        sh.attach_top_left(tl);
        let lg = |p: i32| p as f64 / s as f64;
        sh.set_top_offset(lg(pads[0]));
        sh.set_right_offset(lg(pads[1]));
        sh.set_bottom_offset(lg(pads[2]));
        sh.set_left_offset(lg(pads[3]));
        sh.commit();
    }
    win.shadow = Some(sh);
    win.shadow_scale = win.scale120;
}

fn make_backend(
    kind: super::BackendKind,
    conn: &Connection,
    win: &Win,
) -> Option<Box<dyn crate::render::primitive::Backend>> {
    if kind == super::BackendKind::Software {
        eprintln!("novaui: the Wayland host presents with Vulkan only; ignoring the software backend");
    }
    let (w, h) = win.px();
    let display = conn.backend().display_ptr() as *mut std::ffi::c_void;
    let surface = win.surface.id().as_ptr() as *mut std::ffi::c_void;
    let overlay = matches!(win.role, Role::Layer(_));
    let attach = |v: &mut crate::render::vulkan::Vulkan| {
        v.set_transparent(overlay);
        v.attach(crate::render::vulkan::Window::Wayland { display, surface }, w, h)
    };
    match crate::render::vulkan::Vulkan::new(w, h) {
        Ok(mut v) => match attach(&mut v) {
            Ok(()) if overlay && !v.composes_alpha() => {
                eprintln!("novaui: the Vulkan Wayland surface composes no alpha");
                None
            }
            Ok(()) => Some(Box::new(v)),
            Err(e) => {
                eprintln!("novaui: Vulkan Wayland surface unavailable ({e})");
                None
            }
        },
        Err(e) => {
            eprintln!("novaui: Vulkan unavailable ({e})");
            None
        }
    }
}

/// The Wayland window's part of the turn (`runner::Host`): its connection, surfaces, backend and
/// what its events leave between turns.
struct WlHost {
    conn: Connection,
    qh: QueueHandle<State>,
    st: State,
    win: Win,
    cursors: Cursors,
    data_device: Option<wl_data_device::WlDataDevice>,
    be: Box<dyn Backend>,
    backend: super::BackendKind,
    t0: Instant,
    /// The wake pipe (read end, write end) and whether a wake is unread.
    fds: [c_int; 2],
    pending: Arc<AtomicBool>,
    cursor: Cursor,
    pressed_in_client: bool,
    /// A caption press, and the serial its move needs (`CaptionPress`).
    caption: CaptionPress,
    caption_serial: u32,
    edge: Option<i64>,
    enter_serial: u32,
    /// Key repeat: (code, next fire time).
    repeat: Option<(u32, f64)>,
    /// The window's accessibility tree (AT-SPI), when the app asked for it.
    access: Option<crate::access::Bridge>,
    /// The window is an overlay (a layer surface): where it stands and its drag.
    overlay: Option<Layered>,
}

/// An overlay's place as a layer surface's margins (logical), and the host's own move of it.
struct Layered {
    /// The bottom margin, and where its centre stands from the left once it has been placed
    /// across (`None`: centred by the compositor, anchored at the bottom only).
    bottom: i32,
    centre: Option<i32>,
    /// A caption drag: the pointer's logical position in the surface where it is held.
    drag: Option<(f64, f64)>,
}

/// `Place::x` of an overlay the compositor centres.
const CENTRED: i32 = i32::MIN;

impl Layered {
    /// The place it stands at (`Place`: the bottom centre's distance from the left and bottom
    /// of the output's free area).
    fn place(&self) -> Place {
        Place { x: self.centre.unwrap_or(CENTRED), y: self.bottom }
    }

    /// Anchors and margins to the surface for a `lw`-wide surface.
    fn apply(&self, layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, lw: i32) {
        use zwlr_layer_surface_v1::Anchor;
        match self.centre {
            Some(c) => {
                layer.set_anchor(Anchor::Bottom | Anchor::Left);
                layer.set_margin(0, 0, self.bottom, c - lw / 2);
            }
            None => {
                layer.set_anchor(Anchor::Bottom);
                layer.set_margin(0, 0, self.bottom, 0);
            }
        }
    }
}

impl runner::Host for WlHost {
    fn access(&mut self) -> Option<&mut crate::access::Bridge> {
        self.access.as_mut()
    }

    fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn events(&mut self, ui: &mut Ui, app: &mut dyn App, pacer: &mut Pacer) {
        let hz = self.st.refresh_hz();
        if (runner::Pacer::period_of(hz) - pacer.period).abs() > 1e-6 {
            pacer.set_hz(hz);
        }
        let mut buf = [0u8; 64];
        // SAFETY: the wake pipe's read end, open until `run` returns; at most `buf.len()` bytes.
        if unsafe { libc::read(self.fds[0], buf.as_mut_ptr() as *mut _, buf.len()) } > 0 {
            super::woken(&self.pending, ui);
        }
        let s = self.win.scale();
        let phys = |v: f64| (v * s as f64).round() as i32;

        // Window state and size.
        let mut resized = false;
        if let Some(f) = self.st.p.frac.take() {
            resized |= self.win.set_scale120(f.max(120));
        } else if self.win.viewport.is_none() {
            // The largest integer scale of the outputs the surface is on: of every output before
            // it is on one, as at start (it dropped to 1 there, then came back on entering).
            let sc = self.st.outputs_on().map(|o| o.scale).max().unwrap_or(1).max(1) as u32 * 120;
            resized |= self.win.set_scale120(sc);
        }
        if let Some((size, m, f)) = self.st.p.configure.take() {
            if let Some((w, h)) = size {
                if (w, h) != (self.win.lw, self.win.lh) {
                    (self.win.lw, self.win.lh) = (w, h);
                    resized = true;
                }
            }
            super::set_maximized(ui, m);
            ui.window.fullscreen = f;
            // The size it has when neither maximized nor fullscreen, kept (`Window::bounds`).
            if self.overlay.is_none() {
                let was = ui.window.bounds;
                let bounds = match was {
                    Some(b) if m || f => Some(super::Bounds { maximized: m, ..b }),
                    _ => Some(super::Bounds { x: 0, y: 0, width: self.win.lw, height: self.win.lh, maximized: m }),
                };
                if bounds != was {
                    ui.window.bounds = bounds;
                    ui.events.push(crate::Event::Moved);
                }
            }
        }
        if let Some(sr) = self.st.p.configure_serial.take() {
            if let Role::Toplevel { xdg, .. } = &self.win.role {
                xdg.ack_configure(sr);
            }
        }
        if let Some((serial, w, h)) = self.st.p.layer_configure.take() {
            if let Role::Layer(l) = &self.win.role {
                l.ack_configure(serial);
            }
            if w > 0 && h > 0 && (w as i32, h as i32) != (self.win.lw, self.win.lh) {
                (self.win.lw, self.win.lh) = (w as i32, h as i32);
                resized = true;
            }
        }
        if resized {
            self.win.apply_scale();
            let (w, h) = self.win.px();
            ui.resize(w, h, self.win.scale());
            if self.overlay.is_none() {
                kde_shadow(&self.st, &self.qh, &mut self.win);
            }
        }
        if self.st.p.close {
            ui.window.req.close = true;
        }

        // Pointer.
        if let Some(sr) = self.st.p.pointer_enter_serial.take() {
            self.enter_serial = sr;
            self.cursors.shown = None;
        }
        if std::mem::take(&mut self.st.p.pointer_left) {
            self.edge = None;
            ui.input(Input::Leave);
        }
        let mut pointer_moved = None;
        if let Some((lx, ly)) = self.st.p.pointer {
            let (x, y) = (phys(lx), phys(ly));
            if ui.pointer != Some((x, y)) {
                pointer_moved = Some((x, y));
            }
        }
        if let Some((x, y)) = pointer_moved {
            let e = if self.pressed_in_client { None } else { border_dir(ui, x, y) };
            self.edge = e;
            if let Some(o) = self.overlay.as_mut().filter(|o| o.drag.is_some()) {
                // An overlay follows the held pointer itself: a layer surface has no move.
                if let (Some((hx, hy)), Some((lx, ly))) = (o.drag, self.st.p.pointer) {
                    let (dx, dy) = ((lx - hx).round() as i32, (ly - hy).round() as i32);
                    if (dx, dy) != (0, 0) {
                        let lw = self.win.lw;
                        o.centre = o.centre.or_else(|| self.st.output_size().map(|(w, _)| w / 2)).map(|c| c + dx);
                        o.bottom -= dy;
                        if let Role::Layer(l) = &self.win.role {
                            o.apply(l, lw);
                            self.win.surface.commit();
                        }
                        ui.window.place = Some(o.place());
                        ui.events.push(crate::Event::Moved);
                    }
                }
            } else if let Some((px, py)) = self.caption.moved(x, y) {
                // A caption press that left the slop is a move: the app's press ends here, and
                // the compositor takes the pointer (an overlay's host moves it).
                pointer_taken(ui, &mut self.pressed_in_client, (px, py), MouseButton::Left);
                if let Some(o) = &mut self.overlay {
                    o.drag = Some((px as f64 / s as f64, py as f64 / s as f64));
                } else if let (Some(top), Some(seat)) = (self.win.top(), self.st.seat.as_ref()) {
                    top._move(seat, self.caption_serial);
                }
            } else {
                ui.input(Input::Move(x, y));
            }
        }
        if let Some((lx, ly, serial)) = self.st.p.left_down.take() {
            let (x, y) = (phys(lx), phys(ly));
            let hit = app.nc_hit(ui, x, y);
            let mut taken = false;
            if hit != NcHit::ClientControl {
                if let (Some(d), Some(seat), Some(top)) = (border_dir(ui, x, y), self.st.seat.as_ref(), self.win.top())
                {
                    top.resize(seat, serial, resize_edge(d));
                    taken = true;
                }
            }
            // A press on the caption is the app's first (`CaptionPress`).
            if !taken && hit == NcHit::Caption {
                self.caption.press(x, y, &mut ui.window);
                self.caption_serial = serial;
            }
            if !taken {
                self.pressed_in_client = true;
                ui.input(Input::Mods(self.st.keys.as_ref().map_or_else(Mods::default, Keys::mods)));
                ui.input(Input::Down(x, y, MouseButton::Left));
            }
        }
        if let Some((lx, ly)) = self.st.p.left_up.take() {
            self.caption.release();
            if let Some(o) = &mut self.overlay {
                o.drag = None;
            }
            if self.pressed_in_client {
                self.pressed_in_client = false;
                ui.input(Input::Up(phys(lx), phys(ly), MouseButton::Left));
            }
        }
        for i in std::mem::take(&mut self.st.p.input) {
            // Pointer inputs carry logical coordinates from the handlers.
            ui.input(match i {
                Input::Down(x, y, b) => Input::Down(phys(x as f64), phys(y as f64), b),
                Input::Up(x, y, b) => Input::Up(phys(x as f64), phys(y as f64), b),
                Input::Wheel(x, y, d) => Input::Wheel(phys(x as f64), phys(y as f64), d),
                other => other,
            });
        }
        // A right press on the caption opens the compositor's window menu, which takes the
        // pointer: the app's press ends here.
        if let Some((lx, ly, serial)) = self.st.p.right_down.take() {
            let (x, y) = (phys(lx), phys(ly));
            if let (NcHit::Caption, Some(seat), Some(top)) =
                (app.nc_hit(ui, x, y), self.st.seat.as_ref(), self.win.top())
            {
                ui.input(Input::Up(x, y, MouseButton::Right));
                top.show_window_menu(seat, serial, lx as i32, ly as i32);
            }
        }
        // Key repeat (the client's job on Wayland).
        if let Some(r) = self.st.p.repeat.take() {
            self.repeat = r.and_then(|code| {
                let k = self.st.keys.as_ref()?;
                (k.rate > 0).then_some((code, ui.now + k.delay as f64 / 1000.0))
            });
        }
        if let (Some((code, at)), Some(k)) = (self.repeat, self.st.keys.as_ref()) {
            if ui.now >= at {
                let mut out = Vec::new();
                key_input(k, code, &mut out);
                for i in out {
                    ui.input(i);
                }
                self.repeat = Some((code, at.max(ui.now - 0.1) + 1.0 / k.rate.max(1) as f64));
            }
        }
        if let Some(files) = self.st.p.files.take() {
            if !files.is_empty() {
                ui.input(Input::Files(files));
            }
        }

        if let Some(t) = self.st.p.paste.take() {
            ui.paste(&t);
        }
    }

    fn requests(&mut self, ui: &mut Ui, req: super::Requests) {
        if let (Some(o), Role::Layer(l)) = (&self.overlay, &self.win.role) {
            let mut commit = false;
            if let Some((w, h)) = req.size {
                let (w, h) = (w.round().max(1.0) as i32, h.round().max(1.0) as i32);
                if (w, h) != (self.win.lw, self.win.lh) {
                    l.set_size(w as u32, h as u32);
                    o.apply(l, w);
                    commit = true;
                }
            }
            if let Some(on) = req.click_through {
                // An empty input region: presses go to what is under it (no hover: Wayland
                // gives a client no pointer outside its surfaces).
                match (on, &self.st.compositor) {
                    (true, Some(c)) => {
                        let region = c.create_region(&self.qh, ());
                        self.win.surface.set_input_region(Some(&region));
                        region.destroy();
                    }
                    _ => self.win.surface.set_input_region(None),
                }
                commit = true;
            }
            if commit {
                self.win.surface.commit();
            }
        }
        let Some(top) = self.win.top() else { return };
        if req.minimize {
            top.set_minimized();
        }
        if req.toggle_maximize {
            if ui.window.maximized {
                top.unset_maximized();
            } else {
                top.set_maximized();
            }
        }
        if let Some(f) = req.fullscreen {
            if f != ui.window.fullscreen {
                if f {
                    top.set_fullscreen(None);
                } else {
                    top.unset_fullscreen();
                }
            }
        }
        if let Some(t) = req.title {
            if let Some(a) = &mut self.access {
                a.title = t.clone();
            }
            top.set_title(t);
        }
    }

    fn cursor_and_ime(&mut self, ui: &Ui) {
        self.cursor = ui.cursor;
        if let Some(p) = self.st.pointer.as_ref() {
            let name = match self.edge {
                Some(d) => edge_cursor(resize_edge(d)),
                None => cursor_name(self.cursor),
            };
            self.cursors.set(p, self.enter_serial, name, self.win.buffer_scale() as i32);
        }
        // The input method is on while a text field has the focus, its candidates at the caret.
        if let Some(ti) = self.st.text_input.as_ref() {
            let t = &mut self.st.ti;
            let want = t.entered && ui.ime_on();
            let s = self.win.scale().max(0.01);
            let rect = ui.io.ime_at.filter(|_| want).map(|(x, y, h)| {
                let lg = |v: i32| (v as f32 / s).round() as i32;
                (lg(x), lg(y), 1, lg(h).max(1))
            });
            if want != t.enabled || rect != t.rect {
                if want {
                    if !t.enabled {
                        // `enable` resets the text input's state: all of it follows.
                        ti.enable();
                        ti.set_content_type(
                            zwp_text_input_v3::ContentHint::None,
                            zwp_text_input_v3::ContentPurpose::Normal,
                        );
                    }
                    if let Some((x, y, w, h)) = rect {
                        ti.set_cursor_rectangle(x, y, w, h);
                    }
                } else {
                    ti.disable();
                }
                ti.commit();
                t.enabled = want;
                t.rect = rect;
            }
        }
    }

    fn clipboard_out(&mut self, t: String) {
        if let (Some(m), Some(dd)) = (self.st.ddm.as_ref(), self.data_device.as_ref()) {
            let src = m.create_data_source(&self.qh, ());
            for mime in [TEXT_UTF8, "UTF8_STRING", "text/plain", "TEXT", "STRING"] {
                src.offer(mime.to_string());
            }
            dd.set_selection(Some(&src), self.st.serial);
            self.st.clip = Some(t);
        }
    }

    fn clipboard_in(&mut self) -> Option<String> {
        match &self.st.clip {
            Some(t) => Some(t.clone()),
            None => self.st.sel_offer.as_ref().and_then(|(o, m)| {
                let mime = [TEXT_UTF8, "UTF8_STRING", "text/plain"].into_iter().find(|w| m.iter().any(|x| x == w))?;
                Some(String::from_utf8_lossy(&receive(&self.conn, o, mime)).into_owned())
            }),
        }
    }

    fn picks(&mut self, ui: &mut Ui) {
        super::linux::serve_picks(ui);
    }

    fn edge(&self, ui: &Ui) -> Option<crate::geom::Color> {
        ui.window.framed().then_some(FRAME_EDGE)
    }

    fn backend(&mut self) -> &mut dyn Backend {
        self.be.as_mut()
    }

    fn renew_backend(&mut self) -> bool {
        // The surface takes one presenter at a time: the old device goes before the new one is
        // made on it (as on X11 and Win32).
        let (conn, win, backend) = (&self.conn, &self.win, self.backend);
        runner::replace_backend(&mut self.be, || make_backend(backend, conn, win))
    }
}

/// A connection with its globals bound, and the queue its events come on.
struct Connected {
    conn: Connection,
    q: EventQueue<State>,
    qh: QueueHandle<State>,
    st: State,
    compositor: wl_compositor::WlCompositor,
}

/// Connects to the compositor and binds its globals (two roundtrips: the globals, then the
/// seat's capabilities and the outputs' scales and sizes). `None` without a compositor.
fn connect() -> Option<Connected> {
    let conn = Connection::connect_to_env().ok()?;
    let mut q: EventQueue<State> = conn.new_event_queue();
    let qh = q.handle();
    let mut st = State {
        compositor: None,
        wm: None,
        shm: None,
        seat: None,
        deco: None,
        frac_mgr: None,
        viewporter: None,
        shadow_mgr: None,
        ddm: None,
        ti_mgr: None,
        layer_shell: None,
        xdg_outputs: None,
        text_input: None,
        ti: TextIn::default(),
        outputs: Vec::new(),
        on: Vec::new(),
        pointer: None,
        keyboard: None,
        keys: Keys::new(),
        p: Pending::default(),
        serial: 0,
        dnd_offer: None,
        sel_offer: None,
        offers: Vec::new(),
        dnd_serial: 0,
        clip: None,
        conn: conn.clone(),
    };
    conn.display().get_registry(&qh, ());
    q.roundtrip(&mut st).ok()?;
    let compositor = st.compositor.clone()?;
    st.ask_output_sizes(&qh);
    let _ = q.roundtrip(&mut st);
    Some(Connected { conn, q, qh, st, compositor })
}

/// A surface's viewport and fractional scale, where the compositor has them.
fn scaling(
    st: &State,
    qh: &QueueHandle<State>,
    surface: &wl_surface::WlSurface,
) -> (Option<wp_viewport::WpViewport>, Option<wp_fractional_scale_v1::WpFractionalScaleV1>) {
    let viewport = st.viewporter.as_ref().map(|v| v.get_viewport(surface, qh, ()));
    let frac = match (&st.frac_mgr, &viewport) {
        (Some(m), Some(_)) => Some(m.get_fractional_scale(surface, qh, ())),
        _ => None,
    };
    (viewport, frac)
}

/// Opens the window and runs `app` until it closes. Returns false when no Wayland window could
/// be made (no compositor, no xdg-shell, no Vulkan surface): the caller falls back to X11.
pub(super) fn run(opts: &Options, make: &mut dyn FnMut(&mut Ui) -> Box<dyn App>) -> bool {
    let Some(mut c) = connect() else { return false };
    let Some(wm) = c.st.wm.clone() else {
        eprintln!("novaui: the compositor has no xdg-shell; X11 fallback");
        return false;
    };
    let qh = c.qh.clone();
    let surface = c.compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    top.set_title(opts.title.clone());
    top.set_app_id(opts.title.to_lowercase().replace(' ', "-"));
    // Our own title bar: ask for no server decorations (KWin honours it).
    if let Some(d) = &c.st.deco {
        let td = d.get_toplevel_decoration(&top, &qh, ());
        td.set_mode(zxdg_toplevel_decoration_v1::Mode::ClientSide);
    }
    let (viewport, frac) = scaling(&c.st, &qh, &surface);
    let int_scale = c.st.outputs_on().map(|o| o.scale).max().unwrap_or(1).max(1);
    top.set_min_size(opts.min_width, opts.min_height);
    // Its kept size (a Wayland client has no place on the screen to keep), maximized if it was.
    let kept = opts.bounds.filter(|b| b.width >= opts.min_width && b.height >= opts.min_height);
    if kept.is_some_and(|b| b.maximized) {
        top.set_maximized();
    }
    let (lw, lh) = kept.map_or((opts.width, opts.height), |b| (b.width, b.height));
    let mut win = Win {
        surface,
        role: Role::Toplevel { xdg, top },
        viewport,
        _frac: frac,
        lw,
        lh,
        scale120: int_scale as u32 * 120,
        shadow: None,
        shadow_scale: 0,
    };
    win.surface.commit();
    // The first configure (size, states) and the preferred scale.
    let t_wait = Instant::now();
    while c.st.p.configure_serial.is_none() && t_wait.elapsed() < std::time::Duration::from_secs(3) {
        if c.q.blocking_dispatch(&mut c.st).is_err() {
            return false;
        }
    }
    let _ = c.q.roundtrip(&mut c.st);
    if let Some(f) = c.st.p.frac.take() {
        win.scale120 = f.max(120);
    }
    let mut state = (false, false);
    if let Some((size, m, f)) = c.st.p.configure.take() {
        if let Some((w, h)) = size {
            (win.lw, win.lh) = (w, h);
        }
        state = (m, f);
    }
    if let (Some(s), Role::Toplevel { xdg, .. }) = (c.st.p.configure_serial.take(), &win.role) {
        xdg.ack_configure(s);
    }
    win.apply_scale();
    let Some(be) = make_backend(opts.backend, &c.conn, &win) else {
        return false;
    };
    let access = opts.accessibility.then(|| opts.title.clone());
    host_loop(c, win, be, Opened { backend: opts.backend, access, state, overlay: None }, |ui| make(ui))
}

/// Opens an overlay (`OverlayOptions`) as a layer surface and runs `app` in it until it closes.
/// `Err(make)`, before anything ran, when the compositor has no layer shell (GNOME) or no
/// transparent Vulkan surface: the caller opens it through XWayland.
pub(super) fn run_overlay<F: FnOnce(&mut Ui) -> Box<dyn App>>(opts: &OverlayOptions, make: F) -> Result<(), F> {
    let Some(mut c) = connect() else { return Err(make) };
    let Some(shell) = c.st.layer_shell.clone() else {
        eprintln!("novaui: the compositor has no layer shell; the overlay goes through XWayland");
        return Err(make);
    };
    let qh = c.qh.clone();
    let surface = c.compositor.create_surface(&qh, ());
    let layer =
        shell.get_layer_surface(&surface, None, zwlr_layer_shell_v1::Layer::Overlay, "novaui-overlay".into(), &qh, ());
    let (lw, lh) = (opts.width.round().max(1.0) as i32, opts.height.round().max(1.0) as i32);
    // A kept place on the output (its free area is at most the output), else the default: centred,
    // `lift` of the output's height up.
    let size = c.st.output_size();
    let fits =
        |p: &Place| size.is_none_or(|(w, h)| (p.x == CENTRED || (0..=w).contains(&p.x)) && (0..h).contains(&p.y));
    let placed = match opts.place.filter(fits) {
        Some(p) => Layered { bottom: p.y, centre: (p.x != CENTRED).then_some(p.x), drag: None },
        None => {
            let h = size.map_or(1080, |(_, h)| h);
            Layered { bottom: (opts.lift.clamp(0.0, 1.0) * h as f32).round() as i32, centre: None, drag: None }
        }
    };
    layer.set_size(lw as u32, lh as u32);
    placed.apply(&layer, lw);
    layer.set_exclusive_zone(0);
    layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    if opts.click_through {
        let region = c.compositor.create_region(&qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();
    }
    let (viewport, frac) = scaling(&c.st, &qh, &surface);
    let int_scale = c.st.outputs_on().map(|o| o.scale).max().unwrap_or(1).max(1);
    let mut win = Win {
        surface,
        role: Role::Layer(layer),
        viewport,
        _frac: frac,
        lw,
        lh,
        scale120: int_scale as u32 * 120,
        shadow: None,
        shadow_scale: 0,
    };
    win.surface.commit();
    let t_wait = Instant::now();
    while c.st.p.layer_configure.is_none() && t_wait.elapsed() < std::time::Duration::from_secs(3) {
        if c.q.blocking_dispatch(&mut c.st).is_err() {
            return Err(make);
        }
    }
    let _ = c.q.roundtrip(&mut c.st);
    if let Some(f) = c.st.p.frac.take() {
        win.scale120 = f.max(120);
    }
    if let Some((serial, w, h)) = c.st.p.layer_configure.take() {
        if let Role::Layer(l) = &win.role {
            l.ack_configure(serial);
        }
        if w > 0 && h > 0 {
            (win.lw, win.lh) = (w as i32, h as i32);
        }
    }
    win.apply_scale();
    let Some(be) = make_backend(opts.backend, &c.conn, &win) else {
        if let Role::Layer(l) = &win.role {
            l.destroy();
        }
        win.surface.destroy();
        let _ = c.conn.flush();
        return Err(make);
    };
    let opened = Opened { backend: opts.backend, access: None, state: (false, false), overlay: Some(placed) };
    host_loop(c, win, be, opened, make);
    Ok(())
}

/// What a window opened with besides its surface: its backend's kind, the title its
/// accessibility tree is named by (none: no tree), its first maximized and fullscreen states,
/// and an overlay's place.
struct Opened {
    backend: super::BackendKind,
    access: Option<String>,
    state: (bool, bool),
    overlay: Option<Layered>,
}

/// Runs the app in a window opened (`run`, `run_overlay`) until it closes.
fn host_loop(
    c: Connected,
    win: Win,
    be: Box<dyn Backend>,
    opened: Opened,
    make: impl FnOnce(&mut Ui) -> Box<dyn App>,
) -> bool {
    let Connected { conn, mut q, qh, st, compositor } = c;
    let Opened { backend, access, state: (maximized, fullscreen_on), overlay } = opened;
    let data_device = match (&st.ddm, &st.seat, &overlay) {
        (Some(m), Some(s), None) => Some(m.get_data_device(s, &qh, ())),
        _ => None,
    };
    let mut st = st;
    st.text_input = match (&st.ti_mgr, &st.seat, &overlay) {
        (Some(m), Some(s), None) => Some(m.get_text_input(s, &qh, ())),
        _ => None,
    };
    let cursor_surface = compositor.create_surface(&qh, ());
    let cursors = Cursors {
        theme: st.shm.clone().and_then(|shm| {
            let size = std::env::var("XCURSOR_SIZE").ok().and_then(|v| v.parse().ok()).unwrap_or(24u32);
            wayland_cursor::CursorTheme::load(&conn, shm, size * win.buffer_scale()).ok()
        }),
        surface: cursor_surface,
        shown: None,
    };

    let mut fds = [0 as c_int; 2];
    // Without the check a failed pipe left `fds` at 0 and the flags (and the final close) went
    // to standard input.
    if let Err(e) = super::linux::wake_pipe(&mut fds) {
        eprintln!("novaui: wake pipe: {e}");
        return false;
    }
    let pending = Arc::new(AtomicBool::new(false));
    let waker = HostWake { fd: fds[1], pending: pending.clone() };
    let (pw, ph) = win.px();
    let mut ui = Ui::new(pw, ph, win.scale());
    let task_waker = waker.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || task_waker.wake());
    ui.set_task_wake(wake.clone());
    ui.window.maximized = maximized;
    ui.window.fullscreen = fullscreen_on;
    if overlay.is_none() {
        ui.window.bounds = Some(super::Bounds { x: 0, y: 0, width: win.lw, height: win.lh, maximized });
    }
    if let Some(o) = &overlay {
        ui.window.place = Some(o.place());
        ui.window.work_area = st.output_size().map(|(w, h)| (w as f32, h as f32));
    }
    let mut app = make(&mut ui);
    let is_overlay = overlay.is_some();
    let mut host = WlHost {
        conn,
        qh,
        st,
        win,
        cursors,
        data_device,
        be,
        backend,
        t0: Instant::now(),
        fds,
        pending,
        cursor: Cursor::Arrow,
        pressed_in_client: false,
        caption: CaptionPress::default(),
        caption_serial: 0,
        edge: None,
        enter_serial: 0,
        repeat: None,
        access: access.map(|title| super::linux::access(wake.clone(), title)),
        overlay,
    };
    runner::start(&mut host, &mut ui, app.as_mut());
    if !is_overlay {
        kde_shadow(&host.st, &host.qh, &mut host.win);
    }
    let _ = host.conn.flush();

    let wfd = host.conn.backend().poll_fd().as_raw_fd();
    // A turn that drew nothing waits a refresh period (the compositor's frame callback comes
    // only after a commit, so it cannot pace a turn that committed nothing): the period of the
    // output the window is on, followed as it moves (`WlHost::events`).
    let mut pacer = Pacer::new(host.st.refresh_hz());
    // Work since the heap was last trimmed: trimmed once each time the loop goes idle.
    let mut untrimmed = super::linux::IdleTrim::default();
    loop {
        // Block until Wayland input, a wake, the next frame or a key repeat.
        let _ = q.dispatch_pending(&mut host.st);
        let _ = host.conn.flush();
        let t = runner::Host::now(&host);
        let repeat = host.repeat.map(|(_, at)| runner::secs_until(at, t));
        let timeout = [pacer.timeout(&ui, t, true, false), repeat].into_iter().flatten().reduce(f64::min);
        untrimmed.before_wait(timeout);
        if let Some(guard) = q.prepare_read() {
            let mut p = [
                libc::pollfd { fd: wfd, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: fds[0], events: libc::POLLIN, revents: 0 },
            ];
            // To the microsecond (`runner`): poll's whole milliseconds miss a refresh.
            let ts = runner::timespec(timeout);
            // SAFETY: `p` is a live local array of two pollfds; `ts`, when given, a live timespec.
            unsafe {
                libc::ppoll(
                    p.as_mut_ptr(),
                    2,
                    ts.as_ref().map_or(std::ptr::null(), |t| t as *const _),
                    std::ptr::null(),
                )
            };
            if p[0].revents & libc::POLLIN != 0 {
                let _ = guard.read();
            } else {
                drop(guard);
            }
        }
        if q.dispatch_pending(&mut host.st).is_err() {
            eprintln!("novaui: Wayland connection lost");
            break;
        }
        if !runner::turn(&mut host, &mut ui, app.as_mut(), &mut pacer) {
            break;
        }
    }
    drop(app);
    let WlHost { conn, mut win, be, .. } = host;
    drop(be);
    if let Some(sh) = win.shadow.take() {
        drop_shadow(sh);
    }
    match win.role {
        Role::Toplevel { xdg, top } => {
            top.destroy();
            xdg.destroy();
        }
        Role::Layer(l) => l.destroy(),
    }
    win.surface.destroy();
    let _ = conn.flush();
    // SAFETY: the wake pipe's two descriptors, opened above and closed once, here.
    unsafe {
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
    true
}
