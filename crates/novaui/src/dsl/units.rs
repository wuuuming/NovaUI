//! A text's units, each drawn by the `u-*` expressions (`u-translate`, `u-scale`, `u-opacity`,
//! `u-fg`, `u-rotate`, `u-glow`, `u-outline`) evaluated once per unit with `unit.*` set: `i n t0 t1 dur age p hash start end`. Pure: a unit's
//! look is a function of its timing, its index and the time now (`time`), never of frames drawn.
//! Evaluated only when what they read changed: the text, its `units` / `timing` / `time`, or a
//! binding, let or latch an expression names.

use super::HashSet;
use std::rc::Rc;

use super::{num, resolve_color, Doc, DynProp, Ex, Val};
use crate::style::{UnitFx, UnitSrc, Units};
use crate::{Id, Ui};

/// The `u-*` properties, in `UnitProg::fx` order.
pub(super) const PROPS: [&str; 7] = ["u-translate", "u-scale", "u-opacity", "u-fg", "u-rotate", "u-glow", "u-outline"];

/// One text's `u-*` expressions and what they read.
pub(super) struct UnitProg {
    pub(super) id: Id,
    fx: [Option<Val>; 7],
    names: Vec<String>,
    binds: Vec<String>,
    /// Evaluate again at the next update.
    stale: bool,
    /// The text and source last evaluated for.
    seen: Option<(String, UnitSrc)>,
}

/// `unit.*` for one unit.
#[derive(Clone, Copy)]
pub(super) struct UnitVars {
    i: f32,
    n: f32,
    t0: f32,
    t1: f32,
    time: f32,
    hash: f32,
    /// The whole text's first start and last end.
    start: f32,
    end: f32,
}

impl UnitVars {
    pub(super) fn get(&self, field: &str) -> Option<f32> {
        let dur = self.t1 - self.t0;
        let age = self.time - self.t0;
        Some(match field {
            "i" => self.i,
            "n" => self.n,
            "t0" => self.t0,
            "t1" => self.t1,
            "dur" => dur,
            "age" => age,
            // <0 waiting, 0..1 sung, >1 passed; a unit with no length jumps across.
            "p" => {
                if dur > 1e-4 {
                    age / dur
                } else if age < 0.0 {
                    age
                } else {
                    1.0 + age
                }
            }
            "hash" => self.hash,
            "start" => self.start,
            "end" => self.end,
            _ => return None,
        })
    }
}

/// A stable 0..1 per index (no random draw: the same unit looks the same every run).
fn hash01(i: u32) -> f32 {
    let mut x = i.wrapping_add(0x9e37_79b9).wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    x = x.wrapping_mul(0xc2b2_ae35);
    x ^= x >> 16;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

impl Doc {
    /// Registers `u-*` property `k` of `id` (`prop` hands them here).
    pub(super) fn unit_prop(&mut self, id: Id, k: &str, v: &Val) {
        let Some(slot) = PROPS.iter().position(|p| *p == k) else { return };
        // A bare binding (alone or in a list) is an expression reading it.
        let bare = |v: &Val| match v {
            Val::Bind(b) => Val::Expr(super::Expr(Rc::new(Ex::Bind(b.clone())))),
            o => o.clone(),
        };
        let v = match v {
            Val::List(l) => Val::List(l.iter().map(bare).collect()),
            o => bare(o),
        };
        let i = match self.rec.unit_progs.iter().position(|p| p.id == id) {
            Some(i) => i,
            None => {
                self.rec.unit_progs.push(UnitProg {
                    id,
                    fx: Default::default(),
                    names: Vec::new(),
                    binds: Vec::new(),
                    stale: true,
                    seen: None,
                });
                self.rec.unit_progs.len() - 1
            }
        };
        let p = &mut self.rec.unit_progs[i];
        p.fx[slot] = Some(v);
        let (mut names, mut binds) = (Vec::new(), Vec::new());
        for e in p.fx.iter().flatten() {
            let d = DynProp::new(id, k, e.clone(), None);
            names.extend(d.names);
            binds.extend(d.binds);
        }
        names.retain(|n| !n.starts_with("unit."));
        names.sort();
        names.dedup();
        binds.sort();
        binds.dedup();
        (p.names, p.binds, p.stale) = (names, binds, true);
        self.units_pending = true;
    }

    /// Marks the programs reading a changed binding, let or latch (`all`: the window changed).
    pub(super) fn units_touched(&mut self, changed_binds: &HashSet<String>, moved: &HashSet<String>, all: bool) {
        for p in &mut self.rec.unit_progs {
            if all || p.binds.iter().any(|b| changed_binds.contains(b)) || p.names.iter().any(|n| moved.contains(n)) {
                p.stale = true;
                self.units_pending = true;
            }
        }
    }

    /// Evaluates the programs whose text, source or reads changed.
    pub(super) fn update_units(&mut self, ui: &mut Ui) {
        let version = ui.tree.unit_version;
        if !self.units_pending && version == self.unit_version {
            return;
        }
        self.units_pending = false;
        self.unit_version = version;
        let mut progs = std::mem::take(&mut self.rec.unit_progs);
        progs.retain(|p| ui.tree.alive(p.id));
        for p in &mut progs {
            let el = ui.tree.el(p.id);
            let src = el.unit_src().cloned().unwrap_or_default();
            let text = ui.tree.text_of(p.id).unwrap_or("");
            let same = p.seen.as_ref().is_some_and(|(t, s)| t == text && *s == src);
            if same && !p.stale {
                continue;
            }
            p.stale = false;
            let text = text.to_string();
            let units = self.eval_units(ui, p, &text, &src);
            ui.tree.set_visual(p.id, |v| v.units = units);
            p.seen = Some((text, src));
        }
        progs.append(&mut self.rec.unit_progs);
        self.rec.unit_progs = progs;
    }

    fn eval_units(&mut self, ui: &Ui, p: &UnitProg, text: &str, src: &UnitSrc) -> Option<Rc<Units>> {
        let words = src.words?;
        let list = crate::text::units(text, words, &src.timing);
        if list.len() > UnitFx::MAX_UNITS {
            return None;
        }
        let line = ui.tree.text_line(p.id).unwrap_or(16.0);
        let n = list.len() as f32;
        let (start, end) = (list.first().map_or(0.0, |u| u.1), list.last().map_or(0.0, |u| u.2));
        let mut out = Units { chars: Vec::with_capacity(list.len()), fx: Vec::with_capacity(list.len()) };
        for (i, (r, t0, t1)) in list.into_iter().enumerate() {
            let vars = UnitVars { i: i as f32, n, t0, t1, time: src.time, hash: hash01(i as u32), start, end };
            // Per unit no tween runs (`Eval::call`): nothing to carry back to the doc.
            let mut ev = self.evaluator(0, Some(vars));
            let sheet = ev.sheet;
            let mut eval = |v: &Val| ev.val(v);
            let [tr, sc, op, fg, rot, glow, outline] = &p.fx;
            let mut fx = UnitFx::default();
            if let Some(v) = tr {
                match eval(v) {
                    Val::List(l) if l.len() == 2 => {
                        fx.dx = num(&l[0], sheet);
                        fx.dy = num(&l[1], sheet);
                    }
                    o => fx.dy = num(&o, sheet),
                }
            }
            if let Some(v) = sc {
                // One factor, or `sx, sy`.
                match eval(v) {
                    Val::List(l) if l.len() == 2 => {
                        fx.scale = num(&l[0], sheet);
                        fx.scale_y = num(&l[1], sheet);
                    }
                    o => {
                        fx.scale = num(&o, sheet);
                        fx.scale_y = fx.scale;
                    }
                }
            }
            if let Some(v) = rot {
                fx.rotate = num(&eval(v), sheet);
            }
            if let Some(v) = op {
                fx.opacity = num(&eval(v), sheet);
            }
            if let Some(v) = fg {
                fx.fg = resolve_color(sheet, &eval(v));
            }
            // `size, colour` or `size, colour, strength` (times the colour's alpha).
            let mut halo = |v: &Option<Val>| -> Option<(f32, crate::Color)> {
                let Val::List(l) = eval(v.as_ref()?) else { return None };
                let (r, mut c) = (l.first()?, l.get(1).and_then(|c| resolve_color(sheet, c))?);
                c[3] *= l.get(2).map_or(1.0, |a| num(a, sheet));
                Some((num(r, sheet), c))
            };
            if let Some((r, c)) = halo(glow) {
                (fx.glow, fx.glow_fg) = (r, c);
            }
            if let Some((w, c)) = halo(outline) {
                (fx.outline, fx.outline_fg) = (w, c);
            }
            out.chars.push(r);
            out.fx.push(fx.clamped(line));
        }
        Some(Rc::new(out))
    }
}
