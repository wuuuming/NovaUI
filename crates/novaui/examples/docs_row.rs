//! Row: the sheet shows `gap`, `justify` and `grow`; the last row is filled from Rust with more
//! chips than fit on one line, and `wrap-children` breaks them onto the next.
//!
//! `cargo run -p novaui --example docs_row`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{argb, App, Content, Event, Layout, Len, NcHit, Ui, Visual};

const SHEET: &str = include_str!("docs_row.nui");

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_row.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_row.nui builds");
        let wrap = self.doc.id("wrap");
        for i in 0..12 {
            // Wider and narrower chips, so the lines break at different places.
            let width = if i % 3 == 0 { 96.0 } else { 64.0 };
            let chip = Layout { width: Len::Px(width), height: Len::Px(24.0), ..Layout::default() };
            let look = Visual { bg: Some(argb(0x6660CDFF)), radius: 4.0, ..Visual::default() };
            ui.tree.add(wrap, chip, look, Content::None);
        }
    }

    fn event(&mut self, _ui: &mut Ui, _ev: Event) {}

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Row".into(),
        width: 640,
        height: 344,
        min_width: 400,
        min_height: 300,
        backend: Default::default(),
        accessibility: false,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
