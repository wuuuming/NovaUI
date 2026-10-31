//! Flyout: a "Sort by" menu under its button. One opening is `reset` (the rows go), rows added,
//! `open` against the button; after layout `place` puts it where it fits. A row's click, or a
//! press outside (`Event::PressOutside`), closes it.
//!
//! `cargo run -p novaui --example docs_flyout`
use novaui::dsl::{parse, Doc, Val};
use novaui::platform::{self, Options};
use novaui::widgets::flyout::{Along, Place, Side};
use novaui::widgets::Flyout;
use novaui::{App, Event, Icon, NcHit, Ui};

const SHEET: &str = include_str!("docs_flyout.nui");

/// The rows: an icon, a label, and the action a click on the row sends.
const SORTS: [(Icon, &str, &str); 3] = [
    (Icon::TextFont, "Title", "sort-title"),
    (Icon::Person, "Artist", "sort-artist"),
    (Icon::Clock, "Date added", "sort-date"),
];

pub struct Demo {
    doc: Doc,
    flyout: Flyout,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_flyout.nui parses")), flyout: Flyout::default() }
    }

    fn open(&mut self, ui: &mut Ui) {
        let host = self.doc.id("menu");
        self.flyout.reset(ui, host);
        self.doc.prune(ui);
        for (i, (icon, label, action)) in SORTS.iter().enumerate() {
            let args = [Val::from(*icon), Val::Str((*label).into())];
            let row = self.doc.instantiate(ui, host, None, "MenuItem", &args, &format!("sort{i}")).expect("a row");
            ui.tree.set_action(row, Some(action));
            if i == 1 {
                self.doc.instantiate(ui, host, None, "MenuRule", &[], "rule").expect("a rule");
            }
        }
        let button = self.doc.id("sort");
        self.flyout.open(ui, host, Place::on(button, Side::Below, Along::Start, 4.0), Some(button));
    }

    fn close(&mut self, ui: &mut Ui) {
        let _ = self.flyout.close(ui, self.doc.id("menu"));
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_flyout.nui builds");
        self.doc.set(ui, "sort", "Sort by: Title");
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        match ev {
            Event::Click { action: Some(a), .. } if a.is("open") => self.open(ui),
            Event::Click { action: Some(a), .. } => {
                if let Some((_, label, _)) = SORTS.iter().find(|s| a.is(s.2)) {
                    self.doc.set(ui, "sort", format!("Sort by: {label}"));
                    self.close(ui);
                }
            }
            Event::PressOutside { .. } => self.close(ui),
            _ => {}
        }
    }

    /// Placed after layout: its size is the laid-out rows'.
    fn after_layout(&mut self, ui: &mut Ui) {
        self.flyout.place(ui, self.doc.id("menu"));
        self.doc.update(ui);
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Flyout".into(),
        width: 560,
        height: 280,
        min_width: 400,
        min_height: 240,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
