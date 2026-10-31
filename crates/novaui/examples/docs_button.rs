//! Button: four looks, a disabled one, and a toggle. A click on a button with `on-click` comes
//! to the app as `Event::Click` with the action; the toggle's state is the app's, shown with
//! `set_lit`.
//!
//! `cargo run -p novaui --example docs_button`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{argb, theme, App, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_button.nui");

pub struct Demo {
    doc: Doc,
    shuffle: bool,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_button.nui parses")), shuffle: true }
    }

    fn show_shuffle(&mut self, ui: &mut Ui) {
        ui.tree.set_lit(self.doc.id("shuf"), self.shuffle);
        let token = if self.shuffle { "accent-text" } else { "text-primary" };
        let fg = theme::color(token).unwrap_or(argb(0xFFFFFFFF));
        ui.tree.set_visual(self.doc.id("shuffle"), |v| v.fg = fg);
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_button.nui builds");
        ui.tree.set_disabled(self.doc.id("off"), true);
        self.show_shuffle(ui);
        self.doc.set(ui, "said", "Nothing clicked yet.");
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        let Event::Click { action: Some(a), .. } = ev else { return };
        if a.is("shuffle") {
            self.shuffle = !self.shuffle;
            self.show_shuffle(ui);
        }
        self.doc.set(ui, "said", format!("Clicked: {}", a.name));
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
        title: "Button".into(),
        width: 520,
        height: 180,
        min_width: 400,
        min_height: 180,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
