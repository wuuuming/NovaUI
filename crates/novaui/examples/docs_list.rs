//! VirtualList with a Scrollbar: 100 000 rows, of which a screenful exist. The wheel and the rail
//! both move one offset; the list moves its live rows (no layout), the rail follows.
//!
//! `cargo run -p novaui --example docs_list`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, Options};
use novaui::sched::{Ticker, Timer};
use novaui::widgets::list::VirtualList;
use novaui::widgets::scroll::{Parts, Range, Scrollbar, Skin};
use novaui::{App, Content, Ease, Event, Layout, NcHit, TextContent, Ui, Visual};

const SHEET: &str = include_str!("docs_list.nui");
const COUNT: usize = 100_000;

/// The rail's message: it is moving (a frame) or its hover delay is up (a timer).
struct Rail;

pub struct Demo {
    doc: Doc,
    list: VirtualList,
    rail: Scrollbar,
    ticker: Ticker,
    timer: Timer,
}

impl Demo {
    /// Builds the sheet here, not in `init`: the list is made against its viewport element.
    pub fn new(ui: &mut Ui) -> Self {
        let mut doc = Doc::new(parse(SHEET).expect("docs_list.nui parses"));
        let root = ui.tree.root();
        doc.build(ui, root).expect("docs_list.nui builds");
        let list = VirtualList::new(doc.id("view"), doc.num("pitch"));
        Demo { doc, list, rail: Scrollbar::default(), ticker: ui.ticker(Rail), timer: ui.timer(Rail) }
    }

    /// The live rows for the current offset: new rows are made, rows coming into view filled.
    fn sync_list(&mut self, ui: &mut Ui) {
        let view = self.list.viewport;
        self.list.sync(
            ui,
            COUNT,
            |ui| {
                let row = Layout { padding: [0.0, 16.0, 0.0, 16.0], ..Layout::default() };
                let text = TextContent { size: 14.0, line: 32.0, ..TextContent::default() };
                ui.tree.add(view, row, Visual::default(), Content::Text(text))
            },
            |ui, row, i| ui.tree.set_text(row, &format!("Song number {}", i + 1)),
        );
        self.sync_rail(ui);
    }

    /// The rail's plates for the list's range; it says when it wants to be synced again.
    fn sync_rail(&mut self, ui: &mut Ui) {
        let skin = Skin {
            thin: 2.0,
            width: self.doc.num("rail_width"),
            thumb_width: 6.0,
            minimum: 24.0,
            enter_delay: 0.4,
            leave_delay: 2.0,
            duration: 0.167,
            ease: Ease::Bezier(0.0, 0.0, 0.0, 1.0),
        };
        let parts =
            Parts { root: self.doc.id("rail"), track: self.doc.id("rail-track"), thumb: self.doc.id("rail-thumb") };
        let (_, h) = ui.tree.size(self.list.viewport);
        let range = Range { viewport: h, content: COUNT as f32 * self.list.pitch, offset: self.list.offset };
        let wake = self.rail.sync(ui, parts, range, skin, true, 1.0);
        match wake {
            Some(t) if t <= ui.now() => self.ticker.start(),
            Some(t) => {
                self.ticker.stop();
                self.timer.at(t);
            }
            None => self.ticker.stop(),
        }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        self.doc.set(ui, "count", format!("{COUNT} rows; a screenful exist"));
        self.sync_list(ui);
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        let rail = self.doc.id("rail");
        let offset = match ev {
            Event::Wheel { dy, .. } => {
                self.list.wheel(dy);
                None
            }
            Event::DragStart { id, y, .. } if id == rail => {
                self.rail.press((y - ui.tree.rect(rail).y) as f32 / ui.scale());
                None
            }
            Event::Drag { id, y, .. } if id == rail => self.rail.drag((y - ui.tree.rect(rail).y) as f32 / ui.scale()),
            Event::DragEnd { id, y, .. } if id == rail => {
                self.rail.release((y - ui.tree.rect(rail).y) as f32 / ui.scale())
            }
            Event::DragCancel { id } if id == rail => {
                self.rail.cancel();
                None
            }
            Event::Hover { id, .. } if id == rail => None,
            Event::Message(m) if m.is::<Rail>() => None,
            _ => return,
        };
        if let Some(offset) = offset {
            self.list.offset = offset;
        }
        self.sync_list(ui);
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
        title: "List".into(),
        width: 480,
        height: 360,
        min_width: 320,
        min_height: 240,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
