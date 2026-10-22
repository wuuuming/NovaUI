//! Spacer: the last bar's spacer and right-hand text are added from Rust. A spacer is nothing
//! but a layout that grows.
//!
//! `cargo run -p novaui --example docs_spacer`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Content, Event, Layout, NcHit, TextContent, Ui, Visual};

const SHEET: &str = include_str!("docs_spacer.nui");

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_spacer.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_spacer.nui builds");
        let bar = self.doc.id("built");
        ui.tree.add(bar, Layout { grow: 1.0, ..Layout::default() }, Visual::default(), Content::None);
        let right = TextContent { text: "Right, added from Rust".into(), ..TextContent::default() };
        ui.tree.add(bar, Layout::default(), Visual::default(), Content::Text(right));
    }

    fn event(&mut self, _ui: &mut Ui, _ev: Event) {}

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Spacer".into(),
        width: 560,
        height: 200,
        min_width: 360,
        min_height: 200,
        backend: Default::default(),
        accessibility: false,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
