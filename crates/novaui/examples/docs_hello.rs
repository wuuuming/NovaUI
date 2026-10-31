//! Getting started: one window, its look in `docs_hello.nui`, a button that counts its clicks.
//!
//! `cargo run -p novaui --example docs_hello`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Event, NcHit, Ui};

/// The sheet, compiled into the program.
const SHEET: &str = include_str!("docs_hello.nui");

pub struct Hello {
    doc: Doc,
    clicks: u32,
}

impl Hello {
    pub fn new(_ui: &mut Ui) -> Self {
        Hello { doc: Doc::new(parse(SHEET).expect("docs_hello.nui parses")), clicks: 0 }
    }
}

impl App for Hello {
    /// Builds the sheet under the window's root element and gives `$count` its first value.
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_hello.nui builds");
        self.doc.set(ui, "count", "Not clicked yet.");
    }

    /// Input and messages: a click on an element with `on-click` carries its action.
    fn event(&mut self, ui: &mut Ui, ev: Event) {
        let Event::Click { action: Some(action), .. } = ev else { return };
        match action.name.as_str() {
            "add" => {
                self.clicks += 1;
                self.doc.set(ui, "count", format!("Clicked {} time(s).", self.clicks));
            }
            "close" => ui.window.close(),
            _ => {}
        }
    }

    /// After layout, in every turn that delivered something: the sheet's expressions follow.
    fn after_layout(&mut self, ui: &mut Ui) {
        self.doc.update(ui);
    }

    /// The window has no system frame: the bar drags it, the close button is the app's.
    fn nc_hit(&self, ui: &Ui, x: i32, y: i32) -> NcHit {
        let (bar, close) = (ui.tree.rect(self.doc.id("bar")), ui.tree.rect(self.doc.id("close")));
        if bar.contains(x, y) && !close.contains(x, y) {
            NcHit::Caption
        } else {
            NcHit::Client
        }
    }
}

fn main() {
    let options = Options {
        title: "Hello".into(),
        width: 480,
        height: 240,
        min_width: 320,
        min_height: 200,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Hello::new(ui))).expect("a window");
}
