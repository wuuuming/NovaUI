//! QR: a code for a link, made once and shown in an Image element.
//!
//! `cargo run -p novaui --example docs_qr`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::widgets::qr;
use novaui::{App, Content, Event, ImageContent, NcHit, Ui};

const SHEET: &str = include_str!("docs_qr.nui");
const LINK: &str = "https://example.com/sign-in?code=NOVA-0001";

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_qr.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_qr.nui builds");
        let code = ImageContent { image: qr::image(LINK), opacity: 1.0, fit_square: false, hdr: false };
        ui.tree.set_content(self.doc.id("code"), Content::Image(code));
        self.doc.set(ui, "link", LINK);
    }

    fn event(&mut self, _ui: &mut Ui, _ev: Event) {}

    fn after_layout(&mut self, ui: &mut Ui) {
        self.doc.update(ui);
    }

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "QR".into(),
        width: 520,
        height: 224,
        min_width: 400,
        min_height: 224,
        backend: Default::default(),
        accessibility: false,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
