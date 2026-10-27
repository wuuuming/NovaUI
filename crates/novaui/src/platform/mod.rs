//! Platform hosts. Each owns one window, translates its OS input into `input::Input`, paces frames
//! and drives an `App`. The API is the same on every platform: `run`, dialogs, clipboard. Other threads reach
//! the UI thread through the app's own wakers (`Ui::waker`).

use crate::app::App;

/// The host window as the app sees it: its native handle and state, set by the host, and what
/// the app asks of it, done by the host after the turn.
pub struct Window {
    /// The native window (an HWND on Windows, an X11 window id) for modal dialogs; 0 headless.
    pub(crate) handle: isize,
    pub(crate) maximized: bool,
    pub(crate) fullscreen: bool,
    /// The window has the keyboard.
    pub(crate) focused: bool,
    pub(crate) req: Requests,
    /// An overlay's place as the host last saw it (`Place`); `None` for an ordinary window.
    pub(crate) place: Option<Place>,
    /// The work area (dips) of the display the window is on, where the host knows it.
    pub(crate) work_area: Option<(f32, f32)>,
}

/// The system changed the window's maximized state: recorded, and the layout follows at once.
#[cfg(target_os = "linux")]
pub(crate) fn set_maximized(ui: &mut crate::app::Ui, maximized: bool) {
    if maximized != ui.window.maximized {
        ui.window.maximized = maximized;
        ui.snap_layout();
    }
}

/// A wake the host took (`HostWake`): its pending flag is cleared first, so a wake from now on
/// is seen again, and the app's woken work runs.
pub(crate) fn woken(pending: &std::sync::atomic::AtomicBool, ui: &mut crate::app::Ui) {
    pending.store(false, std::sync::atomic::Ordering::Release);
    ui.woken();
}

impl Default for Window {
    fn default() -> Self {
        Window {
            handle: 0,
            maximized: false,
            fullscreen: false,
            focused: true,
            req: Requests::default(),
            place: None,
            work_area: None,
        }
    }
}

impl Window {
    /// The native window (an HWND on Windows, an X11 window id) for modal dialogs; 0 headless.
    pub fn handle(&self) -> isize {
        self.handle
    }
    pub fn maximized(&self) -> bool {
        self.maximized
    }
    pub fn fullscreen(&self) -> bool {
        self.fullscreen
    }
    /// It has a border: neither maximized nor fullscreen, nor an overlay (the tree draws its
    /// edge, and the resize border is there).
    pub(crate) fn framed(&self) -> bool {
        !self.maximized && !self.fullscreen && self.place.is_none()
    }
    /// The window has the keyboard.
    pub fn focused(&self) -> bool {
        self.focused
    }
    pub fn close(&mut self) {
        self.req.close = true;
    }
    pub fn minimize(&mut self) {
        self.req.minimize = true;
    }
    pub fn toggle_maximize(&mut self) {
        self.req.toggle_maximize = true;
    }
    pub fn set_fullscreen(&mut self, on: bool) {
        self.req.fullscreen = Some(on);
    }
    pub fn set_title(&mut self, title: impl Into<String>) {
        self.req.title = Some(title.into());
    }
    /// Drops what the app asked of the window (a headless driver has none to do it).
    pub fn discard_requests(&mut self) {
        self.req = Requests::default();
    }
    /// An overlay's new size in dips; its bottom centre stays where it is. An ordinary window
    /// ignores it (the user sizes it).
    pub fn set_size(&mut self, width: f32, height: f32) {
        self.req.size = Some((width, height));
    }
    /// An overlay lets the pointer through to what is under it (`true`), or takes it. While it
    /// lets it through it still hears the pointer come and go over it (`Input::Move`,
    /// `Input::Leave`: hover), where the system tells (not on Wayland). An ordinary window
    /// ignores it.
    pub fn set_click_through(&mut self, on: bool) {
        self.req.click_through = Some(on);
    }
    /// Where an overlay stands (`Place`), to keep and hand back the next time it opens; `None`
    /// for an ordinary window. `Event::Moved` says it changed.
    pub fn place(&self) -> Option<Place> {
        self.place
    }
    /// The width and height (dips) of the work area of the display the window is on (the
    /// display less its panels), where the host knows it.
    pub fn work_area(&self) -> Option<(f32, f32)> {
        self.work_area
    }
}

/// Where an overlay stands: its bottom centre, in the host's own measure of the screen
/// (Windows' and X11's desktop pixels, Wayland's distances from its display's edges). It means
/// nothing to the app: it is kept and handed back (`OverlayOptions::place`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub x: i32,
    pub y: i32,
}

/// A window over every other one (desktop lyrics, a notice): no frame, no shadow, no taskbar
/// entry, never activated, transparent where nothing paints (the tree's clear colour; set it
/// transparent). It grows and shrinks about its bottom centre (`Window::set_size`), is moved by
/// the user where the app answers `NcHit::Caption`, and can let the pointer through
/// (`Window::set_click_through`). Opened with `run_overlay` or `spawn_overlay`.
#[derive(Clone, Debug)]
pub struct OverlayOptions {
    pub title: String,
    /// Its first size in dips.
    pub width: f32,
    pub height: f32,
    /// A place an overlay reported before (`Window::place`); `None`, or one on no display now:
    /// centred at the bottom of the main display's work area, `lift` of its height above it.
    pub place: Option<Place>,
    pub lift: f32,
    /// It lets the pointer through from the start.
    pub click_through: bool,
    pub backend: BackendKind,
}

/// Runs the overlay `make` builds on a thread of its own (`novaui-overlay`) until it closes. The
/// app reaches it as any other thread reaches a window: by a channel and the overlay's waker
/// (`Ui::waker`), which `make` can hand out. `Err` when the thread could not start; the
/// thread's result is the overlay's (`run_overlay`).
pub fn spawn_overlay(
    opts: OverlayOptions,
    make: impl FnOnce(&mut crate::app::Ui) -> Box<dyn App> + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<Result<(), crate::Error>>> {
    std::thread::Builder::new().name("novaui-overlay".into()).spawn(move || {
        let result = run_overlay(opts, make);
        if let Err(e) = &result {
            eprintln!("novaui: overlay: {e}");
        }
        result
    })
}

/// The window's 1-px edge where the system draws none (Windows 10, X11, Wayland): WinUI's dark
/// stroke, the colour Windows 11's DWM draws.
pub(crate) const FRAME_EDGE: crate::geom::Color = [0x3A as f32 / 255.0, 0x3A as f32 / 255.0, 0x3A as f32 / 255.0, 1.0];

/// What the app asked of the window this turn.
#[derive(Default)]
pub(crate) struct Requests {
    pub close: bool,
    pub minimize: bool,
    pub toggle_maximize: bool,
    pub fullscreen: Option<bool>,
    pub title: Option<String>,
    /// An overlay's new size (dips).
    pub size: Option<(f32, f32)>,
    pub click_through: Option<bool>,
}

pub struct Options {
    pub title: String,
    pub width: i32,
    pub height: i32,
    pub min_width: i32,
    pub min_height: i32,
    pub backend: BackendKind,
    /// Screen readers can read and operate the window (UI Automation, AT-SPI; `access.rs`).
    /// False: no adapter is made and the system's asks go unanswered.
    pub accessibility: bool,
}

/// Which renderer the window draws with. Windows has all three; Linux Vulkan and software (its
/// Wayland host Vulkan only); one a system cannot give falls back to the next it can.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackendKind {
    /// The system's own: D3D11 on Windows, Vulkan on Linux.
    #[default]
    Default,
    D3d11,
    Vulkan,
    Software,
}

impl BackendKind {
    /// From its short name, as a command line gives it: `d3d11`, `vk` or `sw`.
    pub fn parse(name: &str) -> Option<BackendKind> {
        Some(match name {
            "d3d11" => BackendKind::D3d11,
            "vk" => BackendKind::Vulkan,
            "sw" => BackendKind::Software,
            _ => return None,
        })
    }
}

/// Opens `target`, a URL (`http://`, `https://`) or a file or folder, with what the system opens
/// it with: the user's browser, the file manager, a file's default app. It never waits: the
/// system's opener runs on a thread of its own (`novaui-open`), and a failure is written to
/// stderr.
pub fn open(target: impl Into<std::ffi::OsString>) {
    let target = target.into();
    let spawned = std::thread::Builder::new().name("novaui-open".into()).spawn(move || {
        if let Err(e) = open_now(&target) {
            eprintln!("novaui: could not open {}: {e}", target.to_string_lossy());
        }
    });
    if let Err(e) = spawned {
        eprintln!("novaui: could not open: {e}");
    }
}

/// The part of a window's resize border a point is on (the window draws its own frame). The
/// order is `_NET_WM_MOVERESIZE`'s, the X11 and Wayland hosts' numbering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // each host names the parts it maps
pub(crate) enum FrameBorder {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl FrameBorder {
    /// The part under `(x, y)` of a `w`×`h` window whose border is `thickness` wide, corners
    /// first; `None` inside it.
    pub(crate) fn at(x: i32, y: i32, w: i32, h: i32, thickness: i32) -> Option<Self> {
        let (l, r, t, b) = (x < thickness, x >= w - thickness, y < thickness, y >= h - thickness);
        Some(match (l, r, t, b) {
            (true, _, true, _) => Self::TopLeft,
            (_, true, true, _) => Self::TopRight,
            (true, _, _, true) => Self::BottomLeft,
            (_, true, _, true) => Self::BottomRight,
            (true, ..) => Self::Left,
            (_, true, ..) => Self::Right,
            (_, _, true, _) => Self::Top,
            (.., true) => Self::Bottom,
            _ => return None,
        })
    }
}

#[cfg(windows)]
mod win32;
#[cfg(windows)]
mod win32_hdr;
#[cfg(windows)]
pub use win32::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
mod xim;
#[cfg(target_os = "linux")]
pub use linux::*;

/// Opens the window and runs `app` until it closes: a native Wayland window in a Wayland session
/// (`NOVAUI_X11=1` forces X11/XWayland), else X11. A Wayland session that cannot give a window
/// (no xdg-shell, no Vulkan surface) falls back to X11 through XWayland. `Err` when no window
/// could be had, or no font loads.
#[cfg(target_os = "linux")]
pub fn run(opts: Options, mut make: impl FnMut(&mut crate::app::Ui) -> Box<dyn App>) -> Result<(), crate::Error> {
    crate::text::check_fonts()?;
    linux::prepare_env(wayland::wanted());
    linux::tune_malloc();
    if wayland::wanted() && wayland::run(&opts, &mut make) {
        return Ok(());
    }
    x11::run(opts, make)
}

/// Opens an overlay (`OverlayOptions`) and runs `app` in it on this thread until it closes.
#[cfg(target_os = "linux")]
pub fn run_overlay(
    opts: OverlayOptions,
    make: impl FnOnce(&mut crate::app::Ui) -> Box<dyn App>,
) -> Result<(), crate::Error> {
    crate::text::check_fonts()?;
    // A layer surface where the compositor has the layer shell; else through XWayland (GNOME).
    let make = match wayland::wanted() {
        true => match wayland::run_overlay(&opts, make) {
            Ok(()) => return Ok(()),
            Err(make) => make,
        },
        false => make,
    };
    x11::run_overlay(opts, make)
}

/// The refresh rate to pace turns and tickers by: what the display reports (Win32's display
/// frequency, a RandR mode's `dotClock / (hTotal * vTotal)`, a Wayland output's mHz / 1000),
/// or 60 Hz when it reports nothing usable, under 30 Hz included (frames are paced by the swap
/// chain either way).
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(crate) fn refresh_or_60(reported: Option<f64>) -> f64 {
    reported.filter(|hz| hz.is_finite() && *hz >= 30.0).unwrap_or(60.0)
}

#[cfg(test)]
mod refresh_tests {
    use super::refresh_or_60;

    /// A 144 Hz monitor paces animations at 144 Hz, not the 60 Hz both Linux hosts once
    /// assumed (a ticker stepped every 16.7 ms on it).
    #[test]
    fn the_reported_refresh_paces_turns() {
        // 2560x1440 @ 144 Hz (CVT-RB): 586.59 MHz / (2720 * 1497).
        let hz = refresh_or_60(Some(586_590_000.0 / (2720.0 * 1497.0)));
        assert!((hz - 144.0).abs() < 0.1, "{hz}");
        assert_eq!(refresh_or_60(Some(143_998.0 / 1000.0)), 143.998);
        // Under 30 Hz is nothing usable on every host (Win32 once raised it to 30 instead).
        for nothing in [None, Some(0.0), Some(1.0), Some(24.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_eq!(refresh_or_60(nothing), 60.0);
        }
    }
}
