//! Text-unit expressions are pure. At mount, lower their operands to registers; one set of
//! input registers is refreshed per text, then the same instructions run for each unit.

use super::{arith, num, resolve_color, resolve_num, truth, Decl, Doc, Ex, HashSet, Math, Op, Sheet, Val};
use crate::style::{UnitFx, UnitSrc};
use crate::{Id, Ui};
use std::rc::Rc;

pub(super) const PROPS: [&str; 7] = ["u-translate", "u-scale", "u-opacity", "u-fg", "u-rotate", "u-glow", "u-outline"];
const FIELDS: [&str; 10] = ["i", "n", "t0", "t1", "dur", "age", "p", "hash", "start", "end"];

pub(super) struct UnitProg {
    pub(super) id: Id,
    fx: [Option<Program>; 7],
    stale: bool,
    seen: Option<(String, UnitSrc)>,
    spans: Vec<(std::ops::Range<u32>, f32, f32)>,
}

/// Indices refer to earlier registers. A choice aliases its selected register, even for a
/// string or list: selecting a value need not copy it.
enum Instr {
    Field(usize),
    Not(usize),
    Neg(usize),
    Number(usize),
    Choose(usize, usize, usize),
    Binary(Op, usize, usize),
    Math(Math, Vec<usize>),
    Mix(Vec<usize>),
}

#[derive(Default)]
struct Program {
    values: Vec<Val>,
    which: Vec<usize>,
    code: Vec<(usize, Instr)>,
    /// (register, name, binding rather than a let or window field).
    reads: Vec<(usize, String, bool)>,
    roots: Vec<usize>,
    list: bool,
}

impl Program {
    fn build(doc: &Doc, v: &Val) -> Self {
        let mut p = Self::default();
        let items = if let Val::List(v) = v {
            p.list = true;
            v.as_slice()
        } else {
            std::slice::from_ref(v)
        };
        for v in items {
            let i = match v {
                Val::Expr(e) => p.lower(doc, &e.0),
                Val::Bind(name) => p.read(name, true),
                v => p.constant(doc, v.clone()),
            };
            p.roots.push(i);
        }
        p
    }
    fn slot(&mut self, v: Val) -> usize {
        let i = self.values.len();
        self.values.push(v);
        self.which.push(i);
        i
    }
    fn constant(&mut self, doc: &Doc, v: Val) -> usize {
        let value = if matches!(&v, Val::Ident(s) if s != "none") {
            resolve_num(&doc.sheet, &v)
                .map(Val::Num)
                .or_else(|| resolve_color(&doc.sheet, &v).map(Val::Color))
                .unwrap_or(v)
        } else {
            v
        };
        self.slot(value)
    }
    fn read(&mut self, name: &str, bind: bool) -> usize {
        if let Some((i, _, _)) = self.reads.iter().find(|(_, n, b)| n == name && *b == bind) {
            return *i;
        }
        let i = self.slot(Val::Num(0.0));
        self.reads.push((i, name.to_owned(), bind));
        i
    }
    fn lower(&mut self, doc: &Doc, e: &Ex) -> usize {
        let op = match e {
            Ex::Lit(v) => return self.constant(doc, v.clone()),
            Ex::Bind(n) => return self.read(n, true),
            Ex::Name(n) => {
                if let Some(f) = n.strip_prefix("unit.") {
                    Instr::Field(FIELDS.iter().position(|x| *x == f).unwrap_or(FIELDS.len()))
                } else if matches!(n.as_str(), "window.w" | "window.h" | "window.fullscreen" | "window.maximized")
                    || doc.lets.contains_key(n)
                    || doc.sheet.decls.iter().any(|d| match d {
                        Decl::Let(k, _) | Decl::Latch(k, _, _) => k == n,
                    })
                {
                    return self.read(n, false);
                } else {
                    return self.constant(doc, Val::Ident(n.clone()));
                }
            }
            Ex::Neg(a) => Instr::Neg(self.lower(doc, a)),
            Ex::Not(a) => Instr::Not(self.lower(doc, a)),
            Ex::Bin(op, a, b) => Instr::Binary(*op, self.lower(doc, a), self.lower(doc, b)),
            Ex::Tern(c, a, b) => Instr::Choose(self.lower(doc, c), self.lower(doc, a), self.lower(doc, b)),
            Ex::Call(f, a, _) => {
                if f == "tween" {
                    let Some(a) = a.first() else { return self.slot(Val::Num(0.0)) };
                    Instr::Number(self.lower(doc, a))
                } else {
                    let args = a.iter().map(|a| self.lower(doc, a)).collect();
                    if f == "mix" {
                        Instr::Mix(args)
                    } else {
                        Instr::Math(Math::parse(f), args)
                    }
                }
            }
        };
        let i = self.slot(Val::Num(0.0));
        self.code.push((i, op));
        i
    }
    fn prepare(&mut self, doc: &Doc) {
        for (i, n, bind) in &self.reads {
            let window;
            let v = if *bind {
                doc.values.get(n)
            } else {
                match n.as_str() {
                    "window.w" => {
                        window = Val::Num(doc.win.w);
                        Some(&window)
                    }
                    "window.h" => {
                        window = Val::Num(doc.win.h);
                        Some(&window)
                    }
                    "window.fullscreen" => {
                        window = Val::Bool(doc.win.fullscreen);
                        Some(&window)
                    }
                    "window.maximized" => {
                        window = Val::Bool(doc.win.maximized);
                        Some(&window)
                    }
                    _ => doc.lets.get(n),
                }
            }
            .unwrap_or(&Val::Num(0.0));
            if self.values[*i] != *v {
                self.values[*i].clone_from(v);
            }
        }
    }
    fn run(&mut self, sheet: &Sheet, fields: &[f32; 10]) {
        for (i, op) in &self.code {
            let value = |r: usize| &self.values[self.which[r]];
            let number = |r| num(value(r), sheet);
            let out = match op {
                Instr::Field(f) => Val::Num(fields.get(*f).copied().unwrap_or(0.0)),
                Instr::Neg(a) => Val::Num(-number(*a)),
                Instr::Number(a) => Val::Num(number(*a)),
                Instr::Not(a) => Val::Bool(!truth(value(*a))),
                Instr::Choose(c, a, b) => {
                    self.which[*i] = self.which[if truth(value(*c)) { *a } else { *b }];
                    continue;
                }
                Instr::Binary(op, a, b) => match op {
                    Op::And => Val::Bool(truth(value(*a)) && truth(value(*b))),
                    Op::Or => Val::Bool(truth(value(*a)) || truth(value(*b))),
                    Op::Eq | Op::Ne => Val::Bool(super::equal(sheet, value(*a), value(*b)) == (*op == Op::Eq)),
                    _ => arith(*op, number(*a), number(*b)),
                },
                Instr::Math(f, args) => Val::Num(f.run(args.len(), |i| args.get(i).map_or(0.0, |r| number(*r)))),
                Instr::Mix(args) => {
                    let a = |i| args.get(i).map(|r| value(*r));
                    super::mix(sheet, a(0), a(1), a(2).map_or(0.0, |v| num(v, sheet)))
                }
            };
            self.values[*i] = out;
        }
    }
    fn root(&self, i: usize) -> Option<&Val> {
        self.roots.get(i).map(|r| &self.values[self.which[*r]])
    }
    fn item(&self, i: usize) -> Option<&Val> {
        if !self.list {
            if let Some(Val::List(v)) = self.root(0) {
                return v.get(i);
            }
        }
        self.root(i)
    }
    fn pair(&self, s: &Sheet) -> Option<(f32, f32)> {
        if self.item(2).is_some() {
            return None;
        }
        Some((num(self.item(0)?, s), num(self.item(1)?, s)))
    }
    fn scalar(&self, s: &Sheet) -> f32 {
        if self.list {
            0.0
        } else {
            self.root(0).map_or(0.0, |v| num(v, s))
        }
    }
    fn color(&self, s: &Sheet) -> Option<crate::Color> {
        if !self.list {
            return resolve_color(s, self.root(0)?);
        }
        if self.roots.len() != 2 {
            return None;
        }
        Some(super::with_alpha(resolve_color(s, self.item(0)?)?, resolve_num(s, self.item(1)?)?))
    }
    fn halo(&self, s: &Sheet) -> Option<(f32, crate::Color)> {
        let mut c = resolve_color(s, self.item(1)?)?;
        c[3] *= self.item(2).map_or(1.0, |a| num(a, s));
        Some((num(self.item(0)?, s), c))
    }
}

/// A stable 0..1 per index.
fn hash01(i: u32) -> f32 {
    let mut x = i.wrapping_add(0x9e37_79b9).wrapping_mul(0x85eb_ca6b);
    x ^= x >> 13;
    x = x.wrapping_mul(0xc2b2_ae35);
    x ^= x >> 16;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

impl Doc {
    pub(super) fn unit_prop(&mut self, id: Id, k: &str, v: &Val) {
        let Some(slot) = PROPS.iter().position(|p| *p == k) else { return };
        let compiled = Program::build(self, v);
        let progs = &mut self.rec.unit_progs;
        let i = progs.iter().position(|p| p.id == id).unwrap_or_else(|| {
            progs.push(UnitProg { id, fx: Default::default(), seen: None, stale: true, spans: Vec::new() });
            progs.len() - 1
        });
        progs[i].fx[slot] = Some(compiled);
        progs[i].stale = true;
        self.units_pending = true;
    }
    pub(super) fn units_touched(&mut self, binds: &HashSet<String>, moved: &HashSet<String>, all: bool) {
        for p in &mut self.rec.unit_progs {
            if all
                || p.fx.iter().flatten().flat_map(|f| &f.reads).any(|(_, k, b)| {
                    if *b {
                        binds.contains(k)
                    } else {
                        moved.contains(k)
                    }
                })
            {
                p.stale = true;
                self.units_pending = true;
            }
        }
    }
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
            let Some(src) = ui.tree.el(p.id).unit_src().filter(|src| src.words.is_some()) else {
                if p.seen.take().is_some() {
                    ui.tree.set_visual(p.id, |v| v.units = None);
                }
                continue;
            };
            let text = ui.tree.text_of(p.id).unwrap_or("");
            let same = p.seen.as_ref().is_some_and(|(t, s)| {
                t == text && s.words == src.words && (Rc::ptr_eq(&s.timing, &src.timing) || s.timing == src.timing)
            });
            if same && !p.stale && p.seen.as_ref().is_some_and(|(_, s)| s.time == src.time) {
                continue;
            }
            if !same {
                p.spans = crate::text::units(text, src.words == Some(true), &src.timing);
                p.seen = Some((text.to_owned(), src.clone()));
            } else if let Some((_, s)) = &mut p.seen {
                s.time = src.time;
            }
            p.stale = false;
            if p.spans.len() > UnitFx::MAX_UNITS {
                ui.tree.set_visual(p.id, |v| v.units = None);
                continue;
            }
            let time = src.time;
            let line = ui.tree.text_line(p.id).unwrap_or(16.0);
            for f in p.fx.iter_mut().flatten() {
                f.prepare(self);
            }
            let sheet = &self.sheet;
            ui.tree.edit_units(p.id, |out| {
                let mut changed = !same || out.fx.len() != p.spans.len();
                if changed {
                    out.chars.clear();
                    out.chars.extend(p.spans.iter().map(|s| s.0.clone()));
                    out.fx.resize(p.spans.len(), UnitFx::default());
                }
                let n = p.spans.len() as f32;
                let start = p.spans.first().map_or(0.0, |s| s.1);
                let end = p.spans.last().map_or(0.0, |s| s.2);
                for (i, (_, t0, t1)) in p.spans.iter().enumerate() {
                    let age = time - t0;
                    let dur = t1 - t0;
                    let progress = if dur > 1e-4 {
                        age / dur
                    } else if age < 0.0 {
                        age
                    } else {
                        1.0 + age
                    };
                    let fields = [i as f32, n, *t0, *t1, dur, age, progress, hash01(i as u32), start, end];
                    for f in p.fx.iter_mut().flatten() {
                        f.run(sheet, &fields);
                    }
                    let [tr, sc, op, fg, rot, glow, outline] = &p.fx;
                    let mut fx = UnitFx::default();
                    if let Some(v) = tr {
                        (fx.dx, fx.dy) = v.pair(sheet).unwrap_or_else(|| (0.0, v.scalar(sheet)));
                    }
                    if let Some(v) = sc {
                        (fx.scale, fx.scale_y) = v.pair(sheet).unwrap_or_else(|| (v.scalar(sheet), v.scalar(sheet)));
                    }
                    if let Some(v) = op {
                        fx.opacity = v.scalar(sheet);
                    }
                    if let Some(v) = fg {
                        fx.fg = v.color(sheet);
                    }
                    if let Some(v) = rot {
                        fx.rotate = v.scalar(sheet);
                    }
                    if let Some(pair) = glow.as_ref().and_then(|v| v.halo(sheet)) {
                        (fx.glow, fx.glow_fg) = pair;
                    }
                    if let Some(pair) = outline.as_ref().and_then(|v| v.halo(sheet)) {
                        (fx.outline, fx.outline_fg) = pair;
                    }
                    let fx = fx.clamped(line);
                    changed |= out.fx[i] != fx;
                    out.fx[i] = fx;
                }
                changed
            });
        }
        self.rec.unit_progs = progs;
    }
}
