//! Box: a container with a look. The sheet shows the visual properties; the last tile is built
//! from Rust with the same three parts every element has (a layout, a look, a content).
//!
//! `cargo run -p novaui --example docs_box`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::{argb, theme, App, Content, Event, Layout, Len, NcHit, TextContent, Ui, Visual};

const SHEET: &str = include_str!("docs_box.nui");

pub struct Demo {
    doc: Doc,
    faded: bool,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_box.nui parses")), faded: true }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_box.nui builds");
        let mine = self.doc.id("mine");
        let tile = Layout { width: Len::Px(112.0), height: Len::Px(80.0), ..Layout::default() };
        let look = Visual {
            bg: Some(argb(0xFF6B4FBB)),
            radius: 8.0,
            border: 1.0,
            border_color: argb(0x66FFFFFF),
            ..Visual::default()
        };
        ui.tree.add(mine, tile, look, Content::None);
        let caption = TextContent { text: "built in Rust".into(), size: 12.0, line: 16.0, ..TextContent::default() };
        let fg = theme::color("text-secondary").unwrap_or(argb(0xC5FFFFFF));
        ui.tree.add(mine, Layout::default(), Visual { fg, ..Visual::default() }, Content::Text(caption));
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        if let Event::Click { action: Some(a), .. } = ev {
            if a.is("fade") {
                self.faded = !self.faded;
                ui.tree.set_opacity(self.doc.id("faded"), if self.faded { 0.5 } else { 1.0 });
            }
        }
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Box".into(),
        width: 800,
        height: 152,
        min_width: 400,
        min_height: 152,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
