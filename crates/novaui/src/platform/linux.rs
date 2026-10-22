//! What the Linux hosts (X11, Wayland) share: the wake pipe, the key vocabulary, the window's
//! edge and shadow, and the desktop's pickers and clipboard tools.

use std::os::raw::{c_int, c_long, c_uint, c_ulong};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use x11_dl::keysym as ks;

/// Wakes the host's loop from any thread (one byte into the loop's pipe). The app's own wakers
/// (`Ui::waker`) call it.
#[derive(Clone)]
pub(crate) struct HostWake {
    pub(super) fd: c_int,
    pub(super) pending: Arc<AtomicBool>,
}

impl HostWake {
    pub fn wake(&self) {
        if self.fd >= 0 && !self.pending.swap(true, Ordering::AcqRel) {
            let b = 1u8;
            // SAFETY: one byte from a live local to the wake pipe's write end, open while the host
            // runs; a full pipe or a closed one fails the write, which is harmless (a wake is pending).
            unsafe {
                libc::write(self.fd, &b as *const u8 as *const _, 1);
            }
        }
    }
}

/// Keysym → the key code `Ui` and apps match on (`input::key`).
pub fn vk(sym: c_ulong) -> Option<u32> {
    use crate::input::key;
    let s = sym as c_uint;
    Some(match s {
        ks::XK_a..=ks::XK_z => s - ks::XK_a + key::letter('A'),
        ks::XK_A..=ks::XK_Z => s - ks::XK_A + key::letter('A'),
        ks::XK_0..=ks::XK_9 => key::digit(s - ks::XK_0),
        ks::XK_F1..=ks::XK_F12 => key::function(s - ks::XK_F1 + 1),
        ks::XK_BackSpace => key::BACKSPACE,
        ks::XK_Tab | ks::XK_ISO_Left_Tab => key::TAB,
        ks::XK_Return | ks::XK_KP_Enter => key::ENTER,
        ks::XK_Escape => key::ESCAPE,
        ks::XK_space => key::SPACE,
        ks::XK_Prior => key::PAGE_UP,
        ks::XK_Next => key::PAGE_DOWN,
        ks::XK_End => key::END,
        ks::XK_Home => key::HOME,
        ks::XK_Left => key::LEFT,
        ks::XK_Up => key::UP,
        ks::XK_Right => key::RIGHT,
        ks::XK_Down => key::DOWN,
        ks::XK_Delete => key::DELETE,
        ks::XK_Shift_L | ks::XK_Shift_R => key::SHIFT,
        ks::XK_Control_L | ks::XK_Control_R => key::CONTROL,
        ks::XK_Alt_L | ks::XK_Alt_R => key::ALT,
        0x1008FF26 => key::BROWSER_BACK, // XF86Back
        _ => return None,
    })
}

/// The Fluent window shadow as KWin takes it for a window without decorations: eight
/// premultiplied BGRA tiles (top, top-right, right, bottom-right, bottom, bottom-left, left,
/// top-left) and the paddings (top, right, bottom, left), at `scale`.
/// A tile: width, height, premultiplied BGRA pixels.
pub type ShadowTile = (i32, i32, Vec<u8>);

pub fn shadow_tiles(scale: f32) -> (Vec<ShadowTile>, [i32; 4]) {
    let r = (28.0 * scale).round() as i32; // reach
    let sigma = 12.0 * scale;
    let dy = (6.0 * scale).round() as i32;
    let alpha = 0.42f32;
    // Coverage of a gaussian-blurred edge at distance d outside it.
    let erf = |v: f32| {
        // Abramowitz & Stegun 7.1.26, in f64 (the coefficients' precision).
        let v = v as f64;
        let t = 1.0 / (1.0 + 0.3275911 * v.abs());
        let y = (1.0
            - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592)
                * t
                * (-v * v).exp()) as f32;
        if v >= 0.0 {
            y
        } else {
            -y
        }
    };
    let fall = |d: f32| 0.5 * (1.0 - erf(d / (sigma * std::f32::consts::SQRT_2)));
    // The shadow of a large rect, sampled outside each side: separable falloff.
    let tile = |w: i32, h: i32, f: &dyn Fn(i32, i32) -> f32| -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for yy in 0..h {
            for xx in 0..w {
                let a = (f(xx, yy) * alpha).clamp(0.0, 1.0);
                let v = (a * 255.0).round() as u8;
                px.extend_from_slice(&[0, 0, 0, v]); // BGRA, premultiplied black
            }
        }
        px
    };
    let top_pad = (r - dy).max(1);
    let bot_pad = r + dy;
    let side = r;
    // Distance outside the window edge for each tile's pixel (0 at the window's edge).
    let out_top = |yy: i32| (top_pad - yy) as f32 - 0.5 + dy as f32;
    let out_bot = |yy: i32| yy as f32 + 0.5 - dy as f32;
    let out_l = |xx: i32| (side - xx) as f32 - 0.5;
    let out_r = |xx: i32| xx as f32 + 0.5;
    let lin = |d: f32| fall(d);
    let in_ = 1.0f32;
    type Tile<'a> = (i32, i32, Box<dyn Fn(i32, i32) -> f32 + 'a>);
    let tiles: [Tile; 8] = [
        (1, top_pad, Box::new(move |_, yy| lin(out_top(yy)) * in_)),
        (side, top_pad, Box::new(move |xx, yy| lin(out_top(yy)) * lin(out_r(xx)) * 2.0)),
        (side, 1, Box::new(move |xx, _| lin(out_r(xx)))),
        (side, bot_pad, Box::new(move |xx, yy| lin(out_bot(yy)) * lin(out_r(xx)) * 2.0)),
        (1, bot_pad, Box::new(move |_, yy| lin(out_bot(yy)))),
        (side, bot_pad, Box::new(move |xx, yy| lin(out_bot(yy)) * lin(out_l(xx)) * 2.0)),
        (side, 1, Box::new(move |xx, _| lin(out_l(xx)))),
        (side, top_pad, Box::new(move |xx, yy| lin(out_top(yy)) * lin(out_l(xx)) * 2.0)),
    ];
    let tiles = tiles.into_iter().map(|(w, h, f)| (w, h, tile(w, h, f.as_ref()))).collect();
    (tiles, [top_pad, side, bot_pad, side])
}

/// The window manager took the pointer (a move, the window menu): the app's press of `button`
/// ends at `at`, and no press is held in the client any more.
pub(crate) fn pointer_taken(
    ui: &mut crate::app::Ui,
    pressed_in_client: &mut bool,
    at: (i32, i32),
    button: crate::input::MouseButton,
) {
    ui.input(crate::input::Input::Up(at.0, at.1, button));
    *pressed_in_client = false;
}

/// The `_NET_WM_MOVERESIZE` direction for a point within `EDGE` of the window's border.
/// The resize border under (x, y) of the window, when it has one (`Window::framed`).
pub(crate) fn border_dir(ui: &crate::app::Ui, x: i32, y: i32) -> Option<c_long> {
    let (w, h) = ui.tree.window_size();
    ui.window.framed().then(|| edge_dir(x, y, w, h)).flatten()
}

pub fn edge_dir(x: i32, y: i32, w: i32, h: i32) -> Option<c_long> {
    super::FrameBorder::at(x, y, w, h, EDGE).map(|b| b as c_long)
}

/// Edge thickness for WM resizing, physical px.
pub const EDGE: i32 = 6;

/// GTK's and KDE's default double-click time.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

/// A left press on the app's title bar, as GTK's `GtkWindowHandle` and Chromium's
/// `WindowEventFilterLinux` treat it. The press is the app's first, an ordinary press (it closes
/// a flyout, drops focus); the window manager's move starts only once the held pointer leaves
/// the drag slop; a second press soon after, near the first, toggles maximize.
#[derive(Default)]
pub struct CaptionPress {
    /// Where the button went down, while it is held and the pointer has not left the slop.
    held: Option<(i32, i32)>,
    /// The last single press: when and where, for the double-click.
    last: Option<(std::time::Instant, i32, i32)>,
}

impl CaptionPress {
    /// A left press on the caption; one that completes a double-click toggles maximize.
    pub fn press(&mut self, x: i32, y: i32, window: &mut super::Window) {
        let now = std::time::Instant::now();
        let slop = crate::input::DRAG_SLOP;
        let double = self
            .last
            .is_some_and(|(t, lx, ly)| now - t <= DOUBLE_CLICK && crate::input::within((x, y), (lx, ly), slop));
        self.last = (!double).then_some((now, x, y));
        self.held = (!double).then_some((x, y));
        if double {
            window.toggle_maximize();
        }
    }

    /// The pointer moved with the left button held: the press's point, once it has left the
    /// slop. The host then releases the app's press and hands the move to the window manager.
    pub fn moved(&mut self, x: i32, y: i32) -> Option<(i32, i32)> {
        let (px, py) = self.held?;
        let slop = crate::input::DRAG_SLOP;
        if crate::input::within((x, y), (px, py), slop) {
            return None;
        }
        self.held = None;
        self.last = None;
        Some((px, py))
    }

    /// The left button went up.
    pub fn release(&mut self) {
        self.held = None;
    }
}

/// The preedit an input method shows in the focused field, as both Linux hosts hand it to `Ui`
/// (Wayland's `zwp_text_input_v3`, X11's XIM callbacks): a non-empty one is shown, and an empty
/// one goes to `Ui` only to end one that shows.
#[derive(Default)]
pub(super) struct Preedit {
    shown: bool,
}

impl Preedit {
    /// The input method's preedit now (empty: none) and its caret in chars.
    pub(super) fn set(&mut self, text: String, caret: usize, out: &mut impl FnMut(crate::input::Input)) {
        if !text.is_empty() {
            self.shown = true;
            out(crate::input::Input::Preedit(text, caret));
        } else if std::mem::take(&mut self.shown) {
            out(crate::input::Input::Preedit(String::new(), 0));
        }
    }

    /// A commit ended the preedit (`Input::Commit` clears it in the field), or the field dropped
    /// it with the focus.
    pub(super) fn ended(&mut self) {
        self.shown = false;
    }
}

/// A picker child. kdialog shows the KDE file dialog only when it knows it runs in Plasma; started
/// without the session's variables (from a terminal over ssh, a service, an autostart script) it
/// falls back to Qt's plain dialog, whose "Select Directory" lists files and opens a folder on
/// double-click instead of picking it. A KDE session is recognised by its running plasmashell.
/// A dialog's filters as kdialog and zenity take them: each filter's patterns space-separated.
fn picker_patterns(filters: &[(String, String)]) -> Vec<String> {
    filters.iter().map(|(_, p)| p.replace(';', " ")).collect()
}

fn picker(prog: &str) -> std::process::Command {
    desktop_command(prog)
}

/// A desktop helper (a file picker, `xdg-open`) set up for the session the app runs in.
/// Started outside a login session (from a terminal, a service), the environment can name
/// only the Wayland display: Qt and GTK would then try X11 and abort. They are pointed at
/// Wayland, and at Plasma when it runs (so `xdg-open` picks Dolphin, kdialog its own style).
fn desktop_command(prog: &str) -> std::process::Command {
    let mut c = std::process::Command::new(prog);
    let unset = |k: &str| std::env::var_os(k).is_none_or(|v| v.is_empty());
    if unset("DISPLAY") && !unset("WAYLAND_DISPLAY") {
        if unset("QT_QPA_PLATFORM") {
            c.env("QT_QPA_PLATFORM", "wayland");
        }
        if unset("GDK_BACKEND") {
            c.env("GDK_BACKEND", "wayland");
        }
    }
    if unset("XDG_CURRENT_DESKTOP") && plasma_running() {
        c.env("XDG_CURRENT_DESKTOP", "KDE").env("KDE_FULL_SESSION", "true").env("KDE_SESSION_VERSION", "5");
    }
    c
}

fn plasma_running() -> bool {
    std::fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
        std::fs::read(e.path().join("comm")).is_ok_and(|c| c.strip_suffix(b"\n").unwrap_or(&c) == b"plasmashell")
    })
}

/// `platform::open` on its thread: `xdg-open`, set up for the session, waited for so that it
/// leaves no zombie.
pub(super) fn open_now(target: &std::ffi::OsStr) -> Result<(), String> {
    let status = desktop_command("xdg-open").arg(target).status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("xdg-open: {status}"))
    }
}

/// Starts the pickers the app asked for (`Ui::pick`): each is a child process waited for on a
/// thread of its own, its answer delivered as the app's message; the window keeps turning.
pub(super) fn serve_picks(ui: &mut crate::Ui) {
    let picks = std::mem::take(&mut ui.io.picks);
    if picks.is_empty() {
        return;
    }
    ui.io.waiting.retain(crate::sched::Task::pending);
    for (pick, done, site) in picks {
        let task = ui.sched.spawn_waiting(move |_| pick.answer(open_dialog, save_dialog), done, site);
        ui.io.waiting.push(task);
    }
}

/// The desktop's file picker (kdialog on KDE, zenity elsewhere), run as a child process.
fn open_dialog(folders: bool, multi: bool, filters: &[(String, String)]) -> Vec<PathBuf> {
    let patterns = picker_patterns(filters);
    let attempts: Vec<Vec<String>> = vec![
        {
            let mut a = vec!["kdialog".to_string()];
            if folders {
                a.push("--getexistingdirectory".into());
            } else {
                a.push("--getopenfilename".into());
            }
            a.push(".".into());
            if !folders && !patterns.is_empty() {
                a.push(patterns.join(" "));
            }
            if multi && !folders {
                a.extend(["--multiple".into(), "--separate-output".into()]);
            }
            a
        },
        {
            let mut a = vec!["zenity".to_string(), "--file-selection".into()];
            if folders {
                a.push("--directory".into());
            }
            if multi {
                a.extend(["--multiple".into(), "--separator=\n".into()]);
            }
            a
        },
    ];
    let answer = run_picker(attempts.iter().map(|a| (a[0].as_str(), a[1..].iter().map(String::as_str).collect())));
    answer.map_or_else(Vec::new, |out| out.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
}

/// The answer of the first picker program that starts (`attempts`: a program and its arguments,
/// in order of preference): what it printed, or `None` when it did not succeed (the user
/// cancelled) or none started.
fn run_picker<'a>(attempts: impl IntoIterator<Item = (&'a str, Vec<&'a str>)>) -> Option<String> {
    for (prog, args) in attempts {
        if let Ok(out) = picker(prog).args(&args).output() {
            return out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    None
}

/// The desktop's save picker; a name typed without an extension takes the first filter's (as
/// Windows' picker does).
fn save_dialog(name: &str, filters: &[(String, String)]) -> Option<PathBuf> {
    let patterns = picker_patterns(filters);
    let ext = crate::app::FilePick::save_extension(filters);
    let kde: Vec<&str> = ["--getsavefilename", name].into_iter().chain(patterns.first().map(String::as_str)).collect();
    let attempts = [("kdialog", kde), ("zenity", vec!["--file-selection", "--save", "--filename", name])];
    let s = run_picker(attempts)?.trim().to_string();
    if s.is_empty() {
        return None;
    }
    let mut p = PathBuf::from(s);
    if let (None, Some(ext)) = (p.extension(), ext) {
        p.set_extension(ext);
    }
    Some(p)
}

/// The user's display language as a BCP 47 tag (`zh-CN`, `en-US`) from `LANGUAGE`, `LC_ALL`,
/// `LC_MESSAGES`, `LANG` (`zh_CN.UTF-8` → `zh-CN`); empty if unknown or `C`.
pub fn locale() -> String {
    let v = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
    let v = v.unwrap_or_default();
    let tag = v.split([':', '.', '@']).next().unwrap_or("").replace('_', "-");
    if tag == "C" || tag == "POSIX" {
        String::new()
    } else {
        tag
    }
}

/// glibc's allocator keeps freed memory in its arenas: a decoded 3000×3000 cover (36 MB of
/// RGBA, plus the resize and premultiply buffers) or a page's paragraphs, freed a moment later,
/// stay in the process for good, and the per-thread arenas (decode, tags, assets) each keep
/// their own. Blocks of 256 KiB and up are mapped on their own and so go back when freed; the
/// threshold is pinned so glibc does not raise it after the first large free.
pub fn tune_malloc() {
    // SAFETY: plain values; glibc's allocator tuning, safe to call at any time.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, 256 << 10);
    }
}

/// Gives the heap's free pages back to the system (after a burst: a cover, a page switch).
/// Opens a host's wake pipe into `fds`: both ends non-blocking and closed on exec. On a failure
/// `fds` is left as it was.
pub fn wake_pipe(fds: &mut [c_int; 2]) -> std::io::Result<()> {
    let mut opened = [0 as c_int; 2];
    // SAFETY: `opened` is a live local of the two descriptors pipe writes; then flags only, on
    // the two it opened.
    unsafe {
        if libc::pipe(opened.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in opened {
            libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    *fds = opened;
    Ok(())
}

/// The heap is trimmed once each time the loop goes idle: after work (a turn due at once), before
/// the first wait that is not.
#[derive(Default)]
pub struct IdleTrim {
    untrimmed: bool,
}

impl IdleTrim {
    /// Before the loop waits `timeout` ms (0: not at all).
    pub fn before_wait(&mut self, timeout: c_int) {
        if timeout == 0 {
            self.untrimmed = true;
        } else if std::mem::take(&mut self.untrimmed) {
            trim_heap();
        }
    }
}

pub fn trim_heap() {
    // SAFETY: a plain value; glibc releases only pages it holds free.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// The process environment the Linux hosts need, set by running the program again once (the
/// loader and the Vulkan loader read it only at start, and other threads may already run):
///
/// - Vulkan drivers: the loader opens every installed ICD, and Mesa's (radeon, intel, lavapipe)
///   each pull in libLLVM: ~110 MiB of a ~215 MiB process on an NVIDIA-only machine. Only the
///   ICDs of the GPUs present (PCI vendor in /sys/class/drm) are given to the loader, lavapipe
///   only when none matches. `VK_DRIVER_FILES` / `VK_ICD_FILENAMES` set by the user win.
/// - NVIDIA on Wayland presents through `libnvidia-vulkan-producer.so`, dlopened by that
///   unversioned name, which Debian's 535 packages do not ship: a link in the user's cache and
///   that directory on `LD_LIBRARY_PATH`.
pub fn prepare_env(wayland: bool) {
    if std::env::var_os("NOVAUI_ENV_READY").is_some() {
        return;
    }
    let mut set: Vec<(&str, std::ffi::OsString)> = Vec::new();
    if let Some(files) = vulkan_drivers() {
        set.push(("VK_DRIVER_FILES", files));
    }
    if wayland {
        if let Some(dir) = nvidia_producer_link() {
            let mut path = dir.into_os_string();
            if let Some(old) = std::env::var_os("LD_LIBRARY_PATH").filter(|o| !o.is_empty()) {
                path.push(":");
                path.push(old);
            }
            set.push(("LD_LIBRARY_PATH", path));
        }
    }
    if set.is_empty() {
        return;
    }
    // The resolved path, not /proc/self/exe: the process keeps its own name (argv[0], comm).
    let Ok(exe) = std::env::current_exe() else { return };
    use std::os::unix::process::CommandExt;
    let err =
        std::process::Command::new(exe).args(std::env::args_os().skip(1)).envs(set).env("NOVAUI_ENV_READY", "1").exec();
    eprintln!("novaui: re-exec with the prepared environment failed: {err}");
}

/// The ICD manifests of the GPUs this machine has, joined for `VK_DRIVER_FILES`; `None` when
/// the user chose, nothing is installed, or every installed one matches anyway.
fn vulkan_drivers() -> Option<std::ffi::OsString> {
    if ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"].iter().any(|k| std::env::var_os(k).is_some()) {
        return None;
    }
    let vendors: Vec<String> = std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("device/vendor")).ok())
        .map(|v| v.trim().to_ascii_lowercase())
        .collect();
    let all: Vec<(PathBuf, String)> = ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d", "/usr/local/share/vulkan/icd.d"]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|t| (p, t.to_ascii_lowercase())))
        .collect();
    let is = |t: &str, v: &str| match v {
        "0x10de" => t.contains("nvidia"),
        "0x1002" => t.contains("radeon") || t.contains("amdvlk") || t.contains("amdgpu"),
        "0x8086" => t.contains("intel"),
        _ => false,
    };
    let mut keep: Vec<&PathBuf> =
        all.iter().filter(|(_, t)| vendors.iter().any(|v| is(t, v))).map(|(p, _)| p).collect();
    if keep.is_empty() {
        keep = all.iter().filter(|(_, t)| t.contains("lvp") || t.contains("lavapipe")).map(|(p, _)| p).collect();
    }
    if keep.is_empty() || keep.len() == all.len() {
        return None;
    }
    let mut out = std::ffi::OsString::new();
    for (i, p) in keep.iter().enumerate() {
        if i > 0 {
            out.push(":");
        }
        out.push(p.as_os_str());
    }
    Some(out)
}

/// The directory holding a `libnvidia-vulkan-producer.so` link, when the system lacks the name.
fn nvidia_producer_link() -> Option<PathBuf> {
    const NAME: &str = "libnvidia-vulkan-producer.so";
    let dirs = ["/usr/lib/x86_64-linux-gnu", "/lib/x86_64-linux-gnu", "/usr/lib64", "/usr/lib"];
    if dirs.iter().any(|d| std::path::Path::new(d).join(NAME).exists()) {
        return None;
    }
    let found = ["/usr/lib/x86_64-linux-gnu/nvidia/current", "/usr/lib/x86_64-linux-gnu"]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&format!("{NAME}."))))?;
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = cache.join("novaui").join("nvidia-wayland");
    let link = dir.join(NAME);
    if std::fs::read_link(&link).ok().as_deref() != Some(found.as_path()) {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&found, &link).ok()?;
    }
    Some(dir)
}

/// The window's accessibility on Linux: AT-SPI through `accesskit_unix`, whose handlers run on its
/// own thread. No tree is built until a client (Orca) asks.
pub(super) fn access(wake: Arc<dyn Fn() + Send + Sync>, title: String) -> crate::access::Bridge {
    use crate::access::{Bridge, Handler, Shared};
    let shared = Shared::new(wake);
    let adapter =
        accesskit_unix::Adapter::new(Handler(shared.clone()), Handler(shared.clone()), Handler(shared.clone()));
    Bridge::new(shared, Box::new(AtSpi(adapter)), title)
}

/// The turn's side of `accesskit_unix`'s adapter.
struct AtSpi(accesskit_unix::Adapter);

impl crate::access::Adapter for AtSpi {
    fn update(&mut self, make: &mut dyn FnMut() -> accesskit::TreeUpdate) {
        self.0.update_if_active(make);
    }

    fn window_focus(&mut self, on: bool) {
        self.0.update_window_focus_state(on);
    }
}
