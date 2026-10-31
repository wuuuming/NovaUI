//! Path: outlines as data. The button's outline is a binding: each click gives it the next
//! shape, parsed when it is set (a bad outline draws nothing).
//!
//! `cargo run -p novaui --example docs_path`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_path.nui");

/// Outlines in a 24-unit box: a square, a diamond, a play mark.
const SHAPES: [&str; 3] = ["M5 5 H19 V19 H5 Z", "M12 2 L22 12 L12 22 L2 12 Z", "M7 4 L20 12 L7 20 Z"];

pub struct Demo {
    doc: Doc,
    shape: usize,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_path.nui parses")), shape: 0 }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_path.nui builds");
        self.doc.set(ui, "shape", SHAPES[self.shape]);
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            if a.is("next") {
                self.shape = (self.shape + 1) % SHAPES.len();
                self.doc.set(ui, "shape", SHAPES[self.shape]);
            }
        }
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Path".into(),
        width: 560,
        height: 144,
        min_width: 400,
        min_height: 144,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
