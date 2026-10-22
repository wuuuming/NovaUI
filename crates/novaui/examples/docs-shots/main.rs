//! The documentation site's screenshots, rendered by novaui itself: every recipe (`docs_*.rs`
//! beside this folder, the same files the pages show) is built headless, driven a few turns and
//! drawn by the software renderer (the reference the L1 pixel tests check every GPU path
//! against), then written as a PNG to `website/public/shots/`. Every nova-icons glyph is drawn to
//! `shots/icons/`. Nothing on the site is captured by hand.
//!
//! `cargo run --release -p novaui --example docs-shots [-- <out dir>]`
//!
//! The text is drawn with the bundled Noto fonts (`NOVAUI_FONTS`, or `NOVA_FONTS_DIR` as the
//! tests use it), so the shots are the same on every machine.
use novaui::app::{settle, turn};
use novaui::backend::{frame, Backend, Software};
use novaui::{App, Icon, Input, MouseButton, Ui};
use std::path::{Path, PathBuf};

// Each recipe is an example of its own (`cargo run --example docs_button` opens it in a window);
// here only its `App` is used, and its `main` is not.
#[allow(dead_code)]
#[path = "../docs_box.rs"]
mod docs_box;
#[allow(dead_code)]
#[path = "../docs_button.rs"]
mod docs_button;
#[allow(dead_code)]
#[path = "../docs_column.rs"]
mod docs_column;
#[allow(dead_code)]
#[path = "../docs_custom.rs"]
mod docs_custom;
#[allow(dead_code)]
#[path = "../docs_edit.rs"]
mod docs_edit;
#[allow(dead_code)]
#[path = "../docs_flyout.rs"]
mod docs_flyout;
#[allow(dead_code)]
#[path = "../docs_hello.rs"]
mod docs_hello;
#[allow(dead_code)]
#[path = "../docs_icon.rs"]
mod docs_icon;
#[allow(dead_code)]
#[path = "../docs_image.rs"]
mod docs_image;
#[allow(dead_code)]
#[path = "../docs_list.rs"]
mod docs_list;
#[allow(dead_code)]
#[path = "../docs_material.rs"]
mod docs_material;
#[allow(dead_code)]
#[path = "../docs_path.rs"]
mod docs_path;
#[allow(dead_code)]
#[path = "../docs_qr.rs"]
mod docs_qr;
#[allow(dead_code)]
#[path = "../docs_row.rs"]
mod docs_row;
#[allow(dead_code)]
#[path = "../docs_spacer.rs"]
mod docs_spacer;
#[allow(dead_code)]
#[path = "../docs_stack.rs"]
mod docs_stack;
#[allow(dead_code)]
#[path = "../docs_text.rs"]
mod docs_text;
#[allow(dead_code)]
#[path = "../docs_theme.rs"]
mod docs_theme;

/// The pixels per dip of every shot: crisp on high-density screens, shown at half size.
const SCALE: f32 = 2.0;

struct Shot {
    /// The PNG's name (`<name>.png`), the page's `shots/<name>.png`.
    name: &'static str,
    /// The window, in dips.
    size: (i32, i32),
    make: fn(&mut Ui) -> Box<dyn App>,
    /// Input after the first frame (a hover, a click), at dips; `None`: the first frame as is.
    input: Option<fn(&mut Ui, &mut dyn App)>,
}

fn shots() -> Vec<Shot> {
    vec![
        Shot { name: "hello", size: (480, 240), make: |ui| Box::new(docs_hello::Hello::new(ui)), input: None },
        Shot { name: "box", size: (800, 152), make: |ui| Box::new(docs_box::Demo::new(ui)), input: None },
        Shot { name: "column", size: (862, 244), make: |ui| Box::new(docs_column::Demo::new(ui)), input: None },
        Shot { name: "row", size: (640, 344), make: |ui| Box::new(docs_row::Demo::new(ui)), input: None },
        Shot { name: "spacer", size: (560, 200), make: |ui| Box::new(docs_spacer::Demo::new(ui)), input: None },
        Shot { name: "stack", size: (640, 168), make: |ui| Box::new(docs_stack::Demo::new(ui)), input: None },
        Shot { name: "text", size: (696, 400), make: |ui| Box::new(docs_text::Demo::new(ui)), input: None },
        Shot { name: "icon", size: (520, 260), make: |ui| Box::new(docs_icon::Demo::new(ui)), input: None },
        Shot { name: "path", size: (560, 144), make: |ui| Box::new(docs_path::Demo::new(ui)), input: None },
        Shot { name: "image", size: (656, 232), make: |ui| Box::new(docs_image::Demo::new(ui)), input: None },
        Shot { name: "custom", size: (552, 232), make: |ui| Box::new(docs_custom::Demo::new(ui)), input: None },
        Shot { name: "button", size: (520, 180), make: |ui| Box::new(docs_button::Demo::new(ui)), input: None },
        Shot { name: "edit", size: (400, 300), make: |ui| Box::new(docs_edit::Demo::new(ui)), input: None },
        Shot {
            name: "flyout",
            size: (560, 280),
            make: |ui| Box::new(docs_flyout::Demo::new(ui)),
            input: Some(|ui, app| click(ui, app, 60.0, 40.0)),
        },
        Shot {
            name: "list",
            size: (480, 360),
            make: |ui| Box::new(docs_list::Demo::new(ui)),
            input: Some(|ui, app| wheel(ui, app, 120.0, 200.0, -3.0)),
        },
        Shot { name: "qr", size: (520, 224), make: |ui| Box::new(docs_qr::Demo::new(ui)), input: None },
        Shot { name: "theme", size: (696, 184), make: |ui| Box::new(docs_theme::Demo::new(ui)), input: None },
        Shot { name: "material", size: (792, 318), make: |ui| Box::new(docs_material::Demo::new(ui)), input: None },
    ]
}

/// Dips as shot pixels.
fn px(dips: f32) -> i32 {
    (dips * SCALE) as i32
}

/// Every shot, and the icons, start from the default theme (a recipe may install its own:
/// `docs_theme`).
fn default_theme() {
    novaui::theme::install(novaui::theme::Theme::winui_dark());
}

/// Moves the pointer to (x, y) dips and clicks there, one turn each.
fn click(ui: &mut Ui, app: &mut dyn App, x: f32, y: f32) {
    let (x, y) = (px(x), px(y));
    for ev in [Input::Move(x, y), Input::Down(x, y, MouseButton::Left), Input::Up(x, y, MouseButton::Left)] {
        ui.input(ev);
        turn(app, ui);
    }
}

/// Turns the wheel `notches` at (x, y) dips (negative: toward the end).
fn wheel(ui: &mut Ui, app: &mut dyn App, x: f32, y: f32, notches: f32) {
    let (x, y) = (px(x), px(y));
    ui.input(Input::Move(x, y));
    turn(app, ui);
    ui.input(Input::Wheel(x, y, notches));
    turn(app, ui);
}

/// Moves the pointer to (x, y) dips (a hover).
#[allow(dead_code)]
fn hover(ui: &mut Ui, app: &mut dyn App, x: f32, y: f32) {
    ui.input(Input::Move(px(x), px(y)));
    turn(app, ui);
}

fn render(shot: &Shot) -> (i32, i32, Vec<u32>) {
    let (w, h) = (px(shot.size.0 as f32), px(shot.size.1 as f32));
    default_theme();
    let mut ui = Ui::new(w, h, SCALE);
    let mut app = (shot.make)(&mut ui);
    app.init(&mut ui);
    settle(&mut *app, &mut ui, 0);
    let mut be = Software::new(w, h);
    let now = ui.now();
    frame(&mut ui.tree, &mut be, now);
    if let Some(input) = shot.input {
        input(&mut ui, &mut *app);
    }
    // Long enough for every entrance and hover motion to have arrived.
    ui.set_now(ui.now() + 5.0);
    turn(&mut *app, &mut ui);
    let now = ui.now();
    frame(&mut ui.tree, &mut be, now);
    (w, h, be.readback())
}

/// A frame (`0x__RRGGBB`) as an opaque RGB PNG.
fn write_png(path: &Path, w: i32, h: i32, px: &[u32]) {
    let rgb: Vec<u8> = px.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path).expect("create the PNG"));
    let mut enc = png::Encoder::new(file, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let mut out = enc.write_header().expect("PNG header");
    out.write_image_data(&rgb).expect("PNG data");
}

/// Every glyph at 24 dips, white on the page's dark, one PNG each.
fn icons(dir: &Path) {
    std::fs::create_dir_all(dir).expect("the icons folder");
    default_theme();
    for icon in Icon::ALL {
        let sheet = format!(
            "Box {{ width: 100%; height: 100%; bg: solid-base; align: center; justify: center; \
             Icon {} {{ icon-size: 24; }} }}",
            icon.name()
        );
        let mut doc = novaui::dsl::Doc::new(novaui::dsl::parse(&sheet).expect("the icon sheet"));
        let side = px(40.0);
        let mut ui = Ui::new(side, side, SCALE);
        let root = ui.tree.root();
        doc.build(&mut ui, root).expect("the icon sheet builds");
        let mut be = Software::new(side, side);
        frame(&mut ui.tree, &mut be, 0.0);
        write_png(&dir.join(format!("{}.png", icon.name())), side, side, &be.readback());
    }
}

fn main() {
    // The bundled fonts, so a shot does not depend on the machine's.
    if std::env::var_os("NOVAUI_FONTS").is_none() {
        match std::env::var_os("NOVA_FONTS_DIR") {
            Some(dir) => std::env::set_var("NOVAUI_FONTS", dir),
            None => {
                eprintln!("docs-shots: set NOVAUI_FONTS (or NOVA_FONTS_DIR) to the Noto fonts folder");
                std::process::exit(2);
            }
        }
    }
    let out = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../website/public/shots"));
    std::fs::create_dir_all(&out).expect("the shots folder");
    for shot in shots() {
        let (w, h, px) = render(&shot);
        write_png(&out.join(format!("{}.png", shot.name)), w, h, &px);
        println!("{}.png {}x{}", shot.name, w, h);
    }
    icons(&out.join("icons"));
    println!("icons/ {} glyphs", Icon::ALL.len());
}
