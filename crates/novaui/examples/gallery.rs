//! Every primitive in a real window through the platform host (X11/Vulkan on Linux, Win32/D3D11
//! or `backend=vk` on Windows): text (Latin + CJK), icons, shadows, rounded borders, an image, a
//! layered panel with opacity, the acrylic material over the image, an edit field.
//!
//! With `NOVAUI_GALLERY_OUT=<dir>` it is an end-to-end check: once the scene settles it writes the
//! software reference of the exact tree state (`ref-<n>.ppm`) and prints `READY <n> <window>`; an
//! external capture of the window must equal it. It then clicks its own button (Linux: a
//! synthetic XSendEvent to this window only; the pointer is not moved), which changes text and a
//! layer, and repeats; finally it closes. Without the variable it is an ordinary demo window.
use novaui::backend::{Backend, Software};
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::*;
use std::io::Write;
use std::path::PathBuf;

const SRC: &str = include_str!("gallery.nui");

struct Gallery {
    doc: Doc,
    out: Option<PathBuf>,
    phase: u32,
    /// Waiting for the frame after the reference was taken (the host repaints everything).
    reported: bool,
    /// The next phase's action is due once the external capture had its time.
    act: bool,
    /// Pointer/hover state the reference was rendered with; input after it invalidates it.
    seen: (Option<(i32, i32)>, Vec<Id>),
    clicks: u32,
    next_at: f64,
    /// Polls the scene's state while the capture runs (`out`).
    poll: novaui::Timer,
}

/// The pointer and what it hovers: a reference shot holds while they stay the same.
fn pointer_state(ui: &Ui) -> (Option<(i32, i32)>, Vec<Id>) {
    (ui.pointer(), ui.hovered().to_vec())
}

fn ppm(path: &PathBuf, w: i32, h: i32, px: &[u32]) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    write!(f, "P6\n{w} {h}\n255\n").unwrap();
    for p in px {
        f.write_all(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]).unwrap();
    }
}

impl Gallery {
    fn picture() -> ImageHandle {
        // `NOVAUI_GALLERY_IMAGE=<file>`: that picture instead (a gain-map JPEG shows its HDR
        // rendition on an HDR display).
        if let Some(img) = std::env::var_os("NOVAUI_GALLERY_IMAGE")
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| novaui::image::decode(&b, novaui::image::Fit::Square(400)))
        {
            return img;
        }
        // A deterministic photo stand-in: a smooth two-axis gradient with a disc.
        let (w, h) = (300, 200);
        let mut px = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                let d = ((fx - 0.6).powi(2) + (fy - 0.45).powi(2)).sqrt();
                let disc = if d < 0.22 { 1.0 } else { 0.0 };
                px.extend_from_slice(&[
                    (40.0 + 200.0 * fx + 15.0 * disc) as u8,
                    (60.0 + 120.0 * fy + 60.0 * disc) as u8,
                    (180.0 - 120.0 * fx) as u8,
                    255,
                ]);
            }
        }
        ImageHandle::rgba8(w as i32, h as i32, px)
    }

    /// Sends a left click at (x, y) to this window only, through the X server.
    #[cfg(target_os = "linux")]
    fn click(ui: &Ui, x: i32, y: i32) {
        use x11_dl::xlib;
        let win = ui.window.handle() as u64;
        // SAFETY: a connection of this thread's own, checked open and closed once; each event is a
        // zeroed XEvent with the variant its type names filled in, live for XSendEvent's call.
        std::thread::spawn(move || unsafe {
            let xl = xlib::Xlib::open().unwrap();
            let d = (xl.XOpenDisplay)(std::ptr::null());
            if d.is_null() {
                return;
            }
            for (ty, mask) in [
                (xlib::MotionNotify, xlib::PointerMotionMask),
                (xlib::ButtonPress, xlib::ButtonPressMask),
                (xlib::ButtonRelease, xlib::ButtonReleaseMask),
            ] {
                let mut e: xlib::XEvent = std::mem::zeroed();
                if ty == xlib::MotionNotify {
                    e.motion =
                        xlib::XMotionEvent { type_: ty, window: win, x, y, same_screen: 1, ..std::mem::zeroed() };
                } else {
                    e.button = xlib::XButtonEvent {
                        type_: ty,
                        window: win,
                        x,
                        y,
                        button: 1,
                        same_screen: 1,
                        state: if ty == xlib::ButtonRelease { xlib::Button1Mask } else { 0 },
                        ..std::mem::zeroed()
                    };
                }
                (xl.XSendEvent)(d, win, 1, mask, &mut e);
                (xl.XFlush)(d);
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            (xl.XCloseDisplay)(d);
        });
    }
    #[cfg(not(target_os = "linux"))]
    fn click(ui: &Ui, x: i32, y: i32) {
        let _ = (ui, x, y);
    }
}

impl App for Gallery {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("gallery scene");
        // Debug builds: edit `gallery.nui` while the window is open (F12 inspects).
        self.doc.watch(ui, concat!(env!("CARGO_MANIFEST_DIR"), "/examples/gallery.nui"));
        let pic = self.doc.id("pic");
        ui.tree.set_content(
            pic,
            Content::Image(ImageContent { image: Self::picture(), opacity: 1.0, fit_square: false, hdr: true }),
        );
    }
    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            match a.name.as_str() {
                "close" => ui.window.close(),
                "go" => {
                    self.clicks += 1;
                    let body = self.doc.id("body");
                    ui.tree.set_text(body, &format!("Clicked {} time(s). 已点击。", self.clicks));
                    let panel = self.doc.id("panel");
                    ui.tree.set_opacity(panel, 0.6);
                    ui.tree.set_translate(panel, 40.0, 0.0);
                }
                _ => {}
            }
        }
    }
    /// Every turn that delivered something (and once after `init`).
    fn after_layout(&mut self, ui: &mut Ui) {
        self.doc.update(ui);
        let Some(out) = self.out.clone() else { return };
        // Poll the scene's state; the host otherwise sleeps once nothing is pending.
        self.poll.at(ui.now() + 0.05);
        if ui.tree.has_work() || ui.now() < self.next_at {
            return;
        }
        // The capture of the previous phase is done: act now, then settle again.
        if self.act {
            self.act = false;
            if self.phase == 1 {
                let r = ui.tree.rect(self.doc.id("btn"));
                Self::click(ui, r.x + r.w / 2, r.y + r.h / 2);
            }
            self.next_at = ui.now() + 1.0;
            return;
        }
        if self.phase >= 2 {
            ui.window.close();
            return;
        }
        if !self.reported {
            // Settled: the software reference of exactly this state.
            let (w, h) = (ui.tree.window_size().0, ui.tree.window_size().1);
            let mut r = Software::new(w, h);
            ui.tree.invalidate_all();
            {
                let now = ui.now();
                novaui::backend::frame(&mut ui.tree, &mut r, now)
            };
            ppm(&out.join(format!("ref-{}.ppm", self.phase)), w, h, &r.readback());
            // The real backend repaints everything from scratch next frame.
            ui.tree.invalidate_all();
            self.reported = true;
            self.seen = pointer_state(ui);
            return;
        }
        if self.seen != pointer_state(ui) {
            // The real pointer moved over the window after the reference: take it again.
            self.reported = false;
            return;
        }
        println!("READY {} {} {}x{}", self.phase, ui.window.handle(), ui.tree.window_size().0, ui.tree.window_size().1);
        let _ = std::io::stdout().flush();
        self.reported = false;
        self.phase += 1;
        // The external capture runs now; act again after it.
        self.next_at = ui.now() + 2.0;
        self.act = true;
    }
    fn nc_hit(&self, ui: &Ui, x: i32, y: i32) -> NcHit {
        let bar = ui.tree.rect(self.doc.id("bar"));
        let close = ui.tree.rect(self.doc.id("close"));
        if close.contains(x, y) {
            NcHit::Client
        } else if bar.contains(x, y) {
            NcHit::Caption
        } else {
            NcHit::Client
        }
    }
}

fn main() {
    let backend = std::env::args().nth(1).and_then(|a| novaui::platform::BackendKind::parse(&a)).unwrap_or_default();
    let out = std::env::var_os("NOVAUI_GALLERY_OUT").map(PathBuf::from);
    if let Some(o) = &out {
        std::fs::create_dir_all(o).unwrap();
    }
    platform::run(
        Options {
            title: "NovaUI gallery".into(),
            width: 760,
            height: 480,
            min_width: 400,
            min_height: 300,
            backend,
            accessibility: true,
        },
        |ui| {
            Box::new(Gallery {
                doc: Doc::new(parse(SRC).expect("parse")),
                out: out.clone(),
                phase: 0,
                reported: false,
                act: false,
                seen: (None, Vec::new()),
                clicks: 0,
                next_at: 1.0,
                poll: ui.timer(()),
            })
        },
    )
    .expect("a window");
}
