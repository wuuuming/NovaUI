//! Theming: one component built twice. The left card is built under the default theme (WinUI
//! dark); then the app installs a theme of its own, with a few tokens changed, and builds the
//! right card under it. A sheet resolves names when it is built, so the left card keeps its look.
//!
//! `cargo run -p novaui --example docs_theme`
use novaui::dsl::{parse, Doc, Val};
use novaui::platform::{self, Options};
use novaui::theme::{self, Theme, Tok};
use novaui::{argb, App, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_theme.nui");

/// WinUI dark with a warmer accent, rounder corners and a blue-grey card.
fn warm() -> Theme {
    let mut t = Theme::winui_dark();
    t.set("accent", Tok::Color(argb(0xFFFFB454)))
        .set("card-fill", Tok::Color(argb(0xFF263040)))
        .set("stroke-card", Tok::Color(argb(0xFF3A4A60)))
        .set("radius-control", Tok::Num(16.0))
        .set("radius-overlay", Tok::Num(20.0))
        .set("subtitle", Tok::Type(20.0, 28.0, 700));
    t
}

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_theme.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_theme.nui builds");
        let row = self.doc.id("root");
        self.doc.instantiate(ui, row, None, "Card", &[Val::Str("WinUI dark".into())], "dark").expect("a card");
        theme::install(warm());
        self.doc.instantiate(ui, row, None, "Card", &[Val::Str("An app's own".into())], "warm").expect("a card");
    }

    fn event(&mut self, _ui: &mut Ui, _ev: Event) {}

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Theme".into(),
        width: 696,
        height: 184,
        min_width: 400,
        min_height: 184,
        backend: Default::default(),
        accessibility: false,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
