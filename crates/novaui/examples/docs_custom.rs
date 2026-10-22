//! Custom: an element drawn by a program the app registers, an image effect. The program is
//! given twice, in WGSL for the GPU backends (`docs_custom.wgsl`, compiled when the app is
//! built) and in Rust for the software renderer, and the two must agree within 2/255. Its four parameters are the element's to animate: a click here
//! moves the duotone's amount between none and full.
//!
//! `cargo run -p novaui --example docs_custom`
use novaui::dsl::{parse, Doc};
use novaui::image::effect::{register, EffectPixel, EffectProgram};
use novaui::platform::{self, Options};
use novaui::{App, Content, EffectContent, Event, ImageContent, ImageHandle, NcHit, Ui};

const SHEET: &str = include_str!("docs_custom.nui");

/// Each pixel's luminance mapped from deep blue to warm yellow, mixed with the original by the
/// first parameter (0 to 1). The GPU program is `docs_custom.wgsl`, compiled by the build script
/// (`novaui_shaders::build_effect("docs_duotone", "examples/docs_custom.wgsl", "docs_duotone")`).
static DUOTONE: EffectProgram =
    EffectProgram { name: "docs.duotone", gpu: novaui::effect_shaders!("docs_duotone"), cpu: duotone };

/// The same program on the CPU.
fn duotone(p: &EffectPixel) -> [f32; 4] {
    let c = (p.sample)(p.x / p.w * p.size.0, p.y / p.h * p.size.1);
    let l = (c[0] * 0.299 + c[1] * 0.587 + c[2] * 0.114) / c[3].max(0.0001);
    let (dark, light) = ([0.10, 0.06, 0.32], [1.0, 0.86, 0.45]);
    let k = p.params[0];
    let mut out = [0.0, 0.0, 0.0, c[3]];
    for i in 0..3 {
        let tone = (dark[i] + (light[i] - dark[i]) * l) * c[3];
        out[i] = c[i] + (tone - c[i]) * k;
    }
    out
}

/// A stand-in photo: a two-axis gradient with a disc.
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
    image: ImageHandle,
    amount: f32,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_custom.nui parses")), image: picture(), amount: 1.0 }
    }

    fn tone(&self, ui: &mut Ui) {
        let content = EffectContent {
            effect: register(&DUOTONE),
            image: self.image.clone(),
            params: [self.amount, 0.0, 0.0, 0.0],
            opacity: 1.0,
        };
        ui.tree.set_content(self.doc.id("toned"), Content::Effect(content, None));
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_custom.nui builds");
        let plain = ImageContent { image: self.image.clone(), opacity: 1.0, fit_square: false, hdr: false };
        ui.tree.set_content(self.doc.id("plain"), Content::Image(plain));
        self.tone(ui);
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            if a.is("tone") {
                self.amount = if self.amount > 0.5 { 0.0 } else { 1.0 };
                self.tone(ui);
            }
        }
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Custom".into(),
        width: 552,
        height: 232,
        min_width: 400,
        min_height: 232,
        backend: Default::default(),
        accessibility: false,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
