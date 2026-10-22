//! Shaping: a text cut into runs of one face, script and direction, each run shaped by harfrust
//! (HarfBuzz's rules: ligatures, kerning, contextual and conjunct forms, mark placement), with
//! the text's bidi levels (UAX #9) and line break opportunities (UAX #14). Everything here is in
//! ems, independent of the size, so one shaping serves every size and width a text is laid out
//! at (`Text::shaped` caches it per text and weight).

use super::{Family, HashMap, Pick};
use harfrust::{Buffer, BufferFlags, Direction, Script, ShapeOptions, ShapePlan, ShaperFont, Tag};
use unicode_bidi::BidiClass;
use unicode_linebreak::BreakOpportunity;
use unicode_segmentation::UnicodeSegmentation;

/// Positions come back from harfrust in units of `1 / SCALE` em: finer than the font's own
/// units, so a variable face's fractional advances keep their precision.
const SCALE: i32 = 1 << 16;

/// A shaped glyph: its face and id, the cluster it belongs to (the byte offset of the first char
/// it draws), its advance, and how far it is set off its pen position (right and up), in ems.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glyph {
    pub fam: u8,
    pub face: u8,
    pub id: u16,
    pub cluster: u32,
    pub adv: f32,
    pub dx: f32,
    pub dy: f32,
}

/// A run: one face, script and bidi level. Its glyphs are in visual order as harfrust gives
/// them (right to left reversed), its bytes in the text's order.
#[derive(Clone, Debug)]
pub(crate) struct Run {
    pub bytes: std::ops::Range<u32>,
    pub glyphs: std::ops::Range<u32>,
    pub level: u8,
}

/// What a line is broken by, per cluster in the text's order: where it starts, its advance, and
/// whether a line may (or must) start after it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cluster {
    pub at: u32,
    pub adv: f32,
    /// A space: it never overflows a line, and a line broken after it does not count it.
    pub space: bool,
    /// A hard break (`\n` and its kin): it ends its line, and draws nothing.
    pub hard: bool,
    pub brk: Break,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Break {
    No,
    May,
    Must,
}

/// A text shaped at one weight.
pub(crate) struct Shaped {
    pub glyphs: Vec<Glyph>,
    /// In the text's order.
    pub runs: Vec<Run>,
    pub clusters: Vec<Cluster>,
    /// Paragraphs (cut at hard breaks) as (end byte, base level), in order.
    pub paras: Vec<(u32, u8)>,
}

impl Shaped {
    /// The base level of the paragraph holding byte `at`.
    pub fn para_level(&self, at: u32) -> u8 {
        self.paras.iter().find(|p| at < p.0).or(self.paras.last()).map_or(0, |p| p.1)
    }
}

/// A char that ends its line and is not shaped.
fn hard_break(c: char) -> bool {
    matches!(c, '\n' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Default-ignorable chars (joiners, variation selectors, marks of direction): they go with the
/// face of what they follow and need no glyph of their own.
pub(crate) fn ignorable(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x034F | 0x061C | 0x115F..=0x1160 | 0x17B4..=0x17B5 | 0x180B..=0x180F |
        0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F | 0xFEFF |
        0xFFA0 | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

/// Each byte's bidi level and the paragraphs' base levels. A text with nothing right-to-left in
/// it (the usual case) is level 0 throughout without running the algorithm.
fn bidi(t: &str) -> (Vec<u8>, Vec<(u32, u8)>) {
    use BidiClass::*;
    let plain = t.chars().all(|c| {
        !matches!(unicode_bidi::bidi_class(c), R | AL | AN | RLE | RLO | RLI | FSI | LRE | LRO | LRI | PDF | PDI)
    });
    if plain {
        return (Vec::new(), vec![(t.len() as u32, 0)]);
    }
    let info = unicode_bidi::BidiInfo::new(t, None);
    let levels = info.levels.iter().map(|l| l.number()).collect();
    let paras = info.paragraphs.iter().map(|p| (p.range.end as u32, p.level.number())).collect();
    (levels, paras)
}

/// harfrust's script for `c`'s, `None` for chars of no script of their own (Common, Inherited).
fn script_of(c: char) -> Option<Script> {
    use unicode_script::UnicodeScript;
    match c.script() {
        unicode_script::Script::Common | unicode_script::Script::Inherited | unicode_script::Script::Unknown => None,
        s => {
            let name = s.short_name().as_bytes();
            let tag: [u8; 4] = name.try_into().ok()?;
            Script::from_iso15924_tag(Tag::new(&tag))
        }
    }
}

/// The plans runs are shaped by, per face, script and direction (a plan is the face's lookups
/// compiled for them: the costly part of shaping a short text).
#[derive(Default)]
pub(crate) struct Shaper {
    plans: HashMap<(u8, u8, Option<Script>, bool), ShapePlan>,
    buf: Option<Buffer>,
}

impl Shaper {
    /// Plans name faces by index: a new chain makes them stale.
    pub fn clear(&mut self) {
        self.plans.clear();
    }

    /// Shapes `t` (no `\r`) at `weight` over `families`; `pick` is the face fallback for a
    /// cluster (the first family with every glyph it needs).
    pub fn shape(&mut self, families: &[Family], t: &str, pick: &dyn Fn(&str) -> Pick) -> Shaped {
        let (levels, paras) = bidi(t);
        let level_at = |b: usize| levels.get(b).copied().unwrap_or(0);
        // Each grapheme's face, script and level; hard breaks are left out. A grapheme of no
        // script (spaces, digits, punctuation) takes the script before it, else the one after.
        let mut gs: Vec<(u32, u32, Pick, Option<Script>, u8)> = Vec::new();
        for (b, g) in t.grapheme_indices(true) {
            let c = g.chars().next().unwrap_or(' ');
            if !hard_break(c) {
                gs.push((b as u32, (b + g.len()) as u32, pick(g), script_of(c), level_at(b)));
            }
        }
        let mut last = None;
        for g in gs.iter_mut() {
            g.3 = g.3.or(last);
            last = g.3;
        }
        let mut next = None;
        for g in gs.iter_mut().rev() {
            g.3 = g.3.or(next);
            next = g.3;
        }
        // Runs: graphemes in a row of one face, script and level.
        let mut cuts: Vec<(u32, u32, Pick, Option<Script>, u8)> = Vec::new();
        for g in gs {
            match cuts.last_mut() {
                Some(r) if r.1 == g.0 && (r.2, r.3, r.4) == (g.2, g.3, g.4) => r.1 = g.1,
                _ => cuts.push(g),
            }
        }
        let mut buf = self.buf.take().unwrap_or_default();
        let (mut glyphs, mut runs) = (Vec::new(), Vec::with_capacity(cuts.len()));
        let mut adv = vec![f32::NAN; t.len() + 1];
        for (a, b, (fam, face), script, level) in cuts {
            let f = &families[fam as usize].faces[face as usize];
            let rtl = rtl(level);
            let plan = self.plans.entry((fam, face, script, rtl)).or_insert_with(|| {
                let dir = if rtl { Direction::RightToLeft } else { Direction::LeftToRight };
                ShapePlan::new(&f.shaper, dir, script, None, &[])
            });
            buf.clear();
            for (i, c) in t[a as usize..b as usize].char_indices() {
                buf.push(c as u32, a + i as u32);
            }
            buf.set_direction(plan.direction());
            buf.set_script(plan.script());
            let para_start = paras.iter().rev().find(|p| p.0 <= a).map_or(0, |p| p.0);
            let para_end = paras.iter().find(|p| p.0 >= b).map_or(t.len() as u32, |p| p.0);
            let mut flags = BufferFlags::empty();
            flags.set(BufferFlags::BEGINNING_OF_TEXT, a == para_start);
            flags.set(BufferFlags::END_OF_TEXT, b == para_end);
            buf.set_flags(flags);
            let font = ShaperFont::new(&f.shaper).with_scale(SCALE);
            let shaped = harfrust::shape(&font, &mut buf, ShapeOptions::new().plan(Some(plan)));
            crate::audit::contract!(shaped.is_ok(), "a run is shaped with the plan made for it: {shaped:?}");
            let start = glyphs.len() as u32;
            let em = |v: i32| v as f32 / SCALE as f32;
            if shaped.is_ok() {
                for (gi, gp) in buf.glyph_infos().iter().zip(buf.glyph_positions()) {
                    let g = Glyph {
                        fam,
                        face,
                        id: gi.glyph_id as u16,
                        cluster: gi.cluster,
                        adv: em(gp.x_advance),
                        dx: em(gp.x_offset),
                        dy: em(gp.y_offset),
                    };
                    let a = &mut adv[gi.cluster as usize];
                    *a = if a.is_nan() { g.adv } else { *a + g.adv };
                    glyphs.push(g);
                }
            }
            runs.push(Run { bytes: a..b, glyphs: start..glyphs.len() as u32, level });
        }
        self.buf = Some(buf);
        // Hard breaks are clusters of their own, with no advance.
        for (b, c) in t.char_indices() {
            if hard_break(c) {
                adv[b] = 0.0;
            }
        }
        let mut brk = vec![Break::No; t.len() + 1];
        for (at, op) in unicode_linebreak::linebreaks(t) {
            brk[at] = match op {
                BreakOpportunity::Mandatory => Break::Must,
                BreakOpportunity::Allowed => Break::May,
            };
        }
        let starts: Vec<u32> = (0..t.len()).filter(|&b| !adv[b].is_nan()).map(|b| b as u32).collect();
        let clusters = starts
            .iter()
            .enumerate()
            .map(|(i, &at)| {
                let end = starts.get(i + 1).map_or(t.len(), |&e| e as usize);
                let c = t[at as usize..].chars().next().unwrap_or(' ');
                Cluster { at, adv: adv[at as usize], space: c == ' ', hard: hard_break(c), brk: brk[end] }
            })
            .collect();
        Shaped { glyphs, runs, clusters, paras }
    }
}

/// Glyphs of a line in visual order: the runs' glyphs whose clusters fall in `bytes`, each with
/// its run's level, the runs reordered by UAX #9's rule L2 (from the highest level down to the
/// lowest odd one, every stretch at that level or higher reversed). A run's own glyphs are in
/// visual order already. `extra`: glyphs that follow the line's text (the ellipsis) at a level;
/// each piece says whether it is those.
pub(crate) fn visual<'a>(
    sh: &'a Shaped,
    bytes: std::ops::Range<u32>,
    extra: Option<(&'a [Glyph], u8)>,
) -> Vec<(&'a [Glyph], u8, bool)> {
    let mut pieces: Vec<(&[Glyph], u8, bool)> = Vec::new();
    for r in &sh.runs {
        if r.bytes.end <= bytes.start || r.bytes.start >= bytes.end {
            continue;
        }
        let gs = &sh.glyphs[r.glyphs.start as usize..r.glyphs.end as usize];
        // Clusters run one way through a run's glyphs: the line's part is one stretch of them.
        let inside = |g: &Glyph| bytes.contains(&g.cluster);
        let (Some(i), Some(j)) = (gs.iter().position(inside), gs.iter().rposition(inside)) else { continue };
        pieces.push((&gs[i..=j], r.level, false));
    }
    pieces.extend(extra.map(|(gs, level)| (gs, level, true)));
    let max = pieces.iter().map(|p| p.1).max().unwrap_or(0);
    let min_odd = pieces.iter().map(|p| p.1).filter(|&l| rtl(l)).min();
    if let Some(min_odd) = min_odd {
        for level in (min_odd..=max).rev() {
            let mut i = 0;
            while i < pieces.len() {
                if pieces[i].1 < level {
                    i += 1;
                    continue;
                }
                let j = (i..pieces.len()).find(|&j| pieces[j].1 < level).unwrap_or(pieces.len());
                pieces[i..j].reverse();
                i = j;
            }
        }
    }
    pieces
}

/// A bidi embedding level runs right to left (odd, UAX #9).
pub(crate) fn rtl(level: u8) -> bool {
    level % 2 == 1
}
