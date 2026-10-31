//! Stack: overlapping children. A click on the avatar turns its status dot from "online" to
//! "away" and back: one property of one element, set from Rust.
//!
//! `cargo run -p novaui --example docs_stack`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{argb, App, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_stack.nui");

pub struct Demo {
    doc: Doc,
    away: bool,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_stack.nui parses")), away: false }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_stack.nui builds");
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            if a.is("status") {
                self.away = !self.away;
                let colour = if self.away { argb(0xFFFCE100) } else { argb(0xFF6CCB5F) };
                ui.tree.set_visual(self.doc.id("dot"), |v| v.bg = Some(colour));
            }
        }
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Stack".into(),
        width: 640,
        height: 168,
        min_width: 400,
        min_height: 168,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
