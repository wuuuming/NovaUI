//! Image: an `ImageHandle` made from RGBA pixels and shown three ways. A handle is shared and
//! interned: the three elements hold one texture, freed when the last of them lets it go.
//!
//! `cargo run -p novaui --example docs_image`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Content, Event, ImageContent, ImageHandle, NcHit, Ui};

const SHEET: &str = include_str!("docs_image.nui");

/// A stand-in photo: a two-axis gradient with a disc (a real app decodes a file instead).
fn picture() -> ImageHandle {
    let (w, h) = (300, 200);
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let disc = if (x as f32 - 180.0).hypot(y as f32 - 90.0) < 48.0 { 1.0 } else { 0.0 };
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

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_image.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_image.nui builds");
        let image = picture();
        let show = |image: &ImageHandle, opacity, fit_square| {
            Content::Image(ImageContent { image: image.clone(), opacity, fit_square, hdr: false })
        };
        ui.tree.set_content(self.doc.id("wide"), show(&image, 1.0, false));
        // `fit_square`: the centre square of the image, not the whole image squeezed.
        ui.tree.set_content(self.doc.id("round"), show(&image, 1.0, true));
        ui.tree.set_content(self.doc.id("faint"), show(&image, 0.4, true));
    }

    fn event(&mut self, _ui: &mut Ui, _ev: Event) {}

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Image".into(),
        width: 656,
        height: 232,
        min_width: 400,
        min_height: 232,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
