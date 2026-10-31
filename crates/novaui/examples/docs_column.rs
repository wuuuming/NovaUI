//! Column: the sheet shows `align` and `justify`; the last column's rows are added from Rust,
//! one text element per item of a list.
//!
//! `cargo run -p novaui --example docs_column`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Content, Event, Layout, NcHit, TextContent, Ui, Visual};

const SHEET: &str = include_str!("docs_column.nui");

const SONGS: [&str; 5] = ["Daydream", "Open α Door", "Ref:rain", "AM02:00", "Hz"];

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_column.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_column.nui builds");
        let songs = self.doc.id("songs");
        for (i, title) in SONGS.iter().enumerate() {
            let text = TextContent { text: format!("{}. {title}", i + 1), ..TextContent::default() };
            ui.tree.add(songs, Layout::default(), Visual::default(), Content::Text(text));
        }
    }

    fn event(&mut self, _ui: &mut Ui, _ev: Event) {}

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Column".into(),
        width: 862,
        height: 244,
        min_width: 400,
        min_height: 244,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
