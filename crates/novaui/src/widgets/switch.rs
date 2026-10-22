//! A switch's motion, after WinUI's ToggleSwitch: the knob slides between the track's ends, the
//! track and the knob cross-fade between their off and on looks, and the knob grows under the
//! pointer and stretches while pressed. The product supplies its skin (the nodes' looks and
//! places, and the timings) and its nodes; the motion is paint only (offsets, opacity, scale).
use crate::anim::{Ease, Prop};
use crate::{Id, Ui};

/// The motion's measures. Sizes are the knob's box's scale (`Visual::scale`) and its width's
/// (`Visual::scale_x`): the box is the knob's largest (pressed), the rest are fractions of it.
#[derive(Clone, Copy, Debug)]
pub struct Skin {
    /// How far the knob moves, off to on, in dips.
    pub travel: f32,
    /// The knob's slide (WinUI's RepositionThemeAnimation).
    pub slide: f64,
    pub slide_ease: Ease,
    /// The cross-fade between the off and on looks (linear).
    pub fade: f64,
    /// The knob's change of size.
    pub resize: f64,
    pub resize_ease: Ease,
    /// The knob's `(scale, scale_x)` at rest, under the pointer, pressed.
    pub rest: (f32, f32),
    pub over: (f32, f32),
    pub pressed: (f32, f32),
}

/// The nodes. `root` hears the pointer (`hover-events`, `press-events`), and so may `track`
/// (`press-events`) when it is a press target of its own; `knob` moves (it holds `knob_off` and
/// `knob_on`, the knob's two looks, which take the sizes); `outline` is the track's off look and
/// `fill` its on look. The sheet places `knob` at the end the switch shows.
#[derive(Clone, Copy, Debug)]
pub struct Parts {
    pub root: Id,
    pub track: Id,
    pub knob: Id,
    pub knob_off: Id,
    pub knob_on: Id,
    pub outline: Id,
    pub fill: Id,
}

/// How the pointer has the switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Feel {
    #[default]
    Rest,
    Over,
    Pressed,
}

impl Feel {
    /// As the tree has it: pressed (root or track), under the pointer, or neither.
    pub fn of(ui: &Ui, parts: Parts) -> Feel {
        let (root, track) = (ui.tree.el(parts.root), ui.tree.el(parts.track));
        if root.pressed() || track.pressed() {
            Feel::Pressed
        } else if root.hovered() {
            Feel::Over
        } else {
            Feel::Rest
        }
    }
}

/// What a switch showed, for the one built again in its place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Was {
    pub on: bool,
    pub feel: Feel,
}

fn size(skin: Skin, feel: Feel) -> (f32, f32) {
    match feel {
        Feel::Rest => skin.rest,
        Feel::Over => skin.over,
        Feel::Pressed => skin.pressed,
    }
}

/// A switch just built, showing `on`. With `was` (the one it replaces): from how that one stood,
/// a flip slides the knob from the other end and cross-fades the looks, and the knob keeps the
/// size the pointer gave it. Without, it stands still at rest.
pub fn show(ui: &mut Ui, parts: Parts, skin: Skin, on: bool, was: Option<Was>, now: f64) {
    let tree = &mut ui.tree;
    let (off_look, on_look) = if on { (0.0, 1.0) } else { (1.0, 0.0) };
    let (sc, sx) = size(skin, was.map_or(Feel::Rest, |w| w.feel));
    for k in [parts.knob_off, parts.knob_on] {
        tree.set_prop(k, Prop::Scale, sc);
        tree.set_prop(k, Prop::ScaleX, sx);
    }
    let flipped = was.is_some_and(|w| w.on != on);
    let ends = [(parts.outline, off_look), (parts.knob_off, off_look), (parts.fill, on_look), (parts.knob_on, on_look)];
    for (id, to) in ends {
        tree.set_prop(id, Prop::Opacity, if flipped { 1.0 - to } else { to });
        if flipped {
            tree.animate(id, Prop::Opacity, to, skin.fade as f32, Ease::Linear, now);
        }
    }
    let from = if flipped {
        if on {
            -skin.travel
        } else {
            skin.travel
        }
    } else {
        0.0
    };
    tree.set_prop(parts.knob, Prop::X, from);
    if flipped {
        tree.animate(parts.knob, Prop::X, 0.0, skin.slide as f32, skin.slide_ease, now);
    }
}

/// The pointer's hold on the switch changed (`Event::Hover`, `Event::Pressed` on its root or
/// track): the knob takes the size for it.
pub fn feel(ui: &mut Ui, parts: Parts, skin: Skin, now: f64) {
    let (sc, sx) = size(skin, Feel::of(ui, parts));
    for k in [parts.knob_off, parts.knob_on] {
        ui.tree.animate(k, Prop::Scale, sc, skin.resize as f32, skin.resize_ease, now);
        ui.tree.animate(k, Prop::ScaleX, sx, skin.resize as f32, skin.resize_ease, now);
    }
}
