//! An overlay: a window over every other one, transparent where nothing paints, never
//! activated. Its plate is dragged to move it; hovering shows its buttons; the lock lets the
//! pointer through (hovering then only fades it), and it unlocks itself after five seconds.
//!
//! `cargo run -p novaui --example docs_overlay`
use novaui::dsl::{parse, Doc};
use novaui::platform::{self, OverlayOptions};
use novaui::{App, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_overlay.nui");

pub struct Demo {
    doc: Doc,
    locked: bool,
    unlock: novaui::Timer,
}

/// The lock's timer.
struct Unlock;

impl Demo {
    pub fn new(ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_overlay.nui parses")), locked: false, unlock: ui.timer(Unlock) }
    }

    /// The buttons show while the pointer is over the unlocked overlay; locked, the plate fades.
    fn hover(&mut self, ui: &mut Ui) {
        let over = ui.hovered().contains(&self.doc.id("content"));
        self.doc.set(ui, "bar", if over && !self.locked { 1.0 } else { 0.0 });
        let plate = self.doc.id("plate");
        ui.tree.set_opacity(plate, if over && self.locked { 0.3 } else { 1.0 });
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        // Transparent where nothing paints.
        ui.tree.set_clear([0.0; 4]);
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_overlay.nui builds");
        self.doc.set(ui, "line", "Drag me, or lock me in place");
        self.doc.set(ui, "bar", 0.0);
    }

    fn event(&mut self, ui: &mut Ui, ev: Event) {
        match ev {
            Event::Click { action: Some(a), .. } if a.is("lock") => {
                self.locked = true;
                ui.window.set_click_through(true);
                self.unlock.at(ui.now() + 5.0);
            }
            Event::Click { action: Some(a), .. } if a.is("close") => ui.window.close(),
            Event::Message(m) if m.is::<Unlock>() => {
                self.locked = false;
                ui.window.set_click_through(false);
            }
            Event::Moved => println!("moved to {:?}", ui.window.place()),
            _ => {}
        }
        self.hover(ui);
    }

    /// The window follows its content's laid-out size.
    fn after_layout(&mut self, ui: &mut Ui) {
        self.doc.update(ui);
        let r = ui.tree.rect(self.doc.id("content"));
        let k = ui.scale();
        ui.window.set_size(r.w as f32 / k, r.h as f32 / k);
    }

    /// The plate moves the window; the buttons are buttons.
    fn nc_hit(&self, ui: &Ui, x: i32, y: i32) -> NcHit {
        if ui.tree.rect(self.doc.id("plate")).contains(x, y) {
            NcHit::Caption
        } else {
            NcHit::Client
        }
    }
}

fn main() {
    let options = OverlayOptions {
        title: "Overlay".into(),
        width: 600.0,
        height: 120.0,
        place: None,
        lift: 0.125,
        click_through: false,
        backend: Default::default(),
    };
    platform::run_overlay(options, |ui| Box::new(Demo::new(ui))).expect("an overlay");
}
