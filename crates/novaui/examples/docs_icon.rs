//! Icon: glyphs by name in the sheet; the button's glyph is a binding the app sets with an
//! `Icon` value, stepping through the volume levels on each click.
//!
//! `cargo run -p novaui --example docs_icon`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{App, Event, Icon, NcHit, Ui};

const SHEET: &str = include_str!("docs_icon.nui");

const LEVELS: [Icon; 4] = [Icon::Speaker2, Icon::Speaker1, Icon::Speaker0, Icon::SpeakerMute];

pub struct Demo {
    doc: Doc,
    level: usize,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_icon.nui parses")), level: 0 }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_icon.nui builds");
        self.doc.set(ui, "volume", LEVELS[self.level]);
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            if a.is("volume") {
                self.level = (self.level + 1) % LEVELS.len();
                self.doc.set(ui, "volume", LEVELS[self.level]);
            }
        }
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Icon".into(),
        width: 520,
        height: 260,
        min_width: 400,
        min_height: 260,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
