//! Text: the sheet shows the type ramp, line limits and alignment; `$now` is a binding the app
//! sets. Every click on the window sets it again, and only that element is laid out and painted.
//!
//! `cargo run -p novaui --example docs_text`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_text.nui");

pub struct Demo {
    doc: Doc,
    clicks: u32,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_text.nui parses")), clicks: 0 }
    }

    fn show(&mut self, ui: &mut Ui) {
        let text = format!("Set from Rust {} time(s).", self.clicks + 1);
        self.doc.set(ui, "now", text);
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_text.nui builds");
        self.show(ui);
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            if a.is("again") {
                self.clicks += 1;
                self.show(ui);
            }
        }
    }

    fn after_layout(&mut self, ui: &mut Ui) {
        self.doc.update(ui);
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Text".into(),
        width: 696,
        height: 400,
        min_width: 480,
        min_height: 360,
        backend: Default::default(),
        accessibility: false,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
