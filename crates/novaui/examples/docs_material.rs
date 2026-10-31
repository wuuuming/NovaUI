//! Material 3: one component built under `Theme::material3_dark` and again under
//! `material3_light`. Each theme is extended with the app's state layer colours before it is
//! installed; a sheet resolves names when it is built, so each panel keeps its scheme.
//!
//! `cargo run -p novaui --example docs_material`
use novaui::dsl::{parse, Doc, Val};
use novaui::platform::{self, Options};
use novaui::theme::{self, Theme, Tok};
use novaui::{App, Color, Event, NcHit, Ui};

const SHEET: &str = include_str!("docs_material.nui");

fn colour(t: &Theme, role: &str) -> Color {
    match t.get(&format!("md-sys-color-{role}")) {
        Some(Tok::Color(c)) => c,
        _ => panic!("md-sys-color-{role} is a colour"),
    }
}

fn opacity(t: &Theme, state: &str) -> f32 {
    match t.get(&format!("md-sys-state-{state}-state-layer-opacity")) {
        Some(Tok::Num(o)) => o,
        _ => panic!("md-sys-state-{state}-state-layer-opacity is a number"),
    }
}

/// `over` laid on `under` at `o`.
fn layer(under: Color, over: Color, o: f32) -> Color {
    [0, 1, 2, 3].map(|i| if i == 3 { under[3] } else { under[i] + (over[i] - under[i]) * o })
}

/// The scheme with `app-<name>-hover` / `-press` for each kind of button: its container with its
/// content colour laid over (a text button's container is transparent: the layer alone).
fn with_states(mut t: Theme) -> Theme {
    let kinds = [
        ("primary", Some("primary"), "on-primary"),
        ("tonal", Some("secondary-container"), "on-secondary-container"),
        ("fab", Some("primary-container"), "on-primary-container"),
        ("text", None, "primary"),
    ];
    for (name, container, content) in kinds {
        let over = colour(&t, content);
        for state in ["hover", "pressed"] {
            let o = opacity(&t, state);
            let c = match container {
                Some(role) => layer(colour(&t, role), over, o),
                None => [over[0], over[1], over[2], o],
            };
            let suffix = if state == "hover" { "hover" } else { "press" };
            t.set(&format!("app-{name}-{suffix}"), Tok::Color(c));
        }
    }
    t
}

pub struct Demo {
    doc: Doc,
}

impl Demo {
    pub fn new(_ui: &mut Ui) -> Self {
        Demo { doc: Doc::new(parse(SHEET).expect("docs_material.nui parses")) }
    }
}

impl App for Demo {
    fn init(&mut self, ui: &mut Ui) {
        let root = ui.tree.root();
        self.doc.build(ui, root).expect("docs_material.nui builds");
        let row = self.doc.id("root");
        theme::install(with_states(Theme::material3_dark()));
        self.doc.instantiate(ui, row, None, "Showcase", &[Val::Str("Material 3, dark".into())], "dark").expect("dark");
        theme::install(with_states(Theme::material3_light()));
        self.doc
            .instantiate(ui, row, None, "Showcase", &[Val::Str("Material 3, light".into())], "light")
            .expect("light");
    }

    fn event(&mut self, _ui: &mut Ui, _ev: Event) {}

    fn nc_hit(&self, _ui: &Ui, _x: i32, _y: i32) -> NcHit {
        NcHit::Client
    }
}

fn main() {
    let options = Options {
        title: "Material 3".into(),
        width: 792,
        height: 318,
        min_width: 420,
        min_height: 300,
        backend: Default::default(),
        accessibility: false,
        bounds: None,
    };
    platform::run(options, |ui| Box::new(Demo::new(ui))).expect("a window");
}
