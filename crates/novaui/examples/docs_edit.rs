//! Edit: three fields. The middle one's `on-edit` action carries its changes (`Event::Edit`)
//! and Enter (`Event::Submit`); its text is set from Rust at start. The last one is secret.
//!
//! `cargo run -p novaui --example docs_edit`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_edit.nui");

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_edit.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_edit.nui builds");
        let name = self.doc.id("name");
        ui.set_edit_text(name, "My playlist");
        ui.set_edit_secret(self.doc.id("secret"), true);
        ui.set_edit_text(self.doc.id("secret"), "hunter2");
        // The middle field starts with the keyboard.
        ui.set_focus(Some(name));
        self.doc.set(ui, "said", "Type, then press Enter.");
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        match ev {
            Event::Edit { action: Some(a), text, .. } if a.is("rename") => {
                self.doc.set(ui, "said", format!("Editing: {text}"));
            }
            Event::Submit { action: Some(a), text, .. } if a.is("rename") => {
                self.doc.set(ui, "said", format!("Saved as \u{201C}{text}\u{201D}."));
            }
            Event::Cancel { action, .. } if action.is("rename") => {
                self.doc.set(ui, "said", "Left without saving.");
            }
            _ => {}
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
        title: "Edit".into(),
        width: 400,
        height: 300,
        min_width: 340,
        min_height: 280,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
