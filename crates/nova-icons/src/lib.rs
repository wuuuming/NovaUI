//! The kernel's icons: Fluent UI System Icons outlines, rasterized on the CPU into A8 coverage.
//!
//! Neither app draws an icon from a font. Segoe Fluent Icons (Windows 11) and Segoe MDL2 Assets
//! (Windows 10) draw the same code point differently and exist nowhere else, so every icon is an
//! outline compiled into this crate and filled here, the same bytes on every platform.
//!
//! # Using it
//!
//! * [`Icon`] names every icon; [`Icon::key`] is the character that stands for it inside text
//!   (painter lays out a text run of the icon family as icons, one em box per key), and
//!   [`Icon::from_key`] maps it back. Keys of icons that replaced a Segoe glyph ARE that glyph's
//!   old code point, so text that carried the glyph carries the icon unchanged; icons with no
//!   Segoe ancestor take keys from U+F0000 (plane-15 private use).
//! * [`rasterize`] (or [`Rasterizer::rasterize_into`], which reuses its buffers) fills an icon at
//!   an em size and a pixel density and returns a [`CoverageTile`]: `w × h` coverage bytes, where
//!   the tile sits relative to the em box, the exact ink bounds for centring, and the advance.
//!   Colour is not part of the tile — multiply it in at draw time (painter's glyph quads, or an
//!   egui texture of white with this alpha, tinted by the mesh colour), so recolouring never
//!   rasterizes again. [`metrics`] answers the same geometry without filling.
//!
//! # Placement
//!
//! An icon occupies one em box: `size` wide (the advance) and `size` tall, its top where a Segoe
//! glyph's ascent put it. Inside the box the outline is uniformly scaled to fit, and centred on, a
//! per-icon placement box: for an icon that replaced a Segoe glyph, the ink box that glyph had
//! (`segoe-boxes.tsv`), so sizes and positions stay where they were; for the others, the outline's
//! own ink box in its 24-unit design grid. Of the 16/20/24 masters an outline ships, the one whose
//! design grid lands closest to one unit per pixel is filled, so small sizes use the outline drawn
//! for them.
//!
//! # A product's own icons
//!
//! [`Icon`] is the framework's table: general Fluent UI System Icons outlines, one per key. A
//! product that needs a glyph the framework does not ship compiles its own table (the same shapes,
//! in its own build) and installs it with [`install_icons`]; [`glyph_for_key`] then resolves its
//! keys exactly like the framework's, so the text engine lays them out and rasterizes them with no
//! framework change. Installed tables are process-wide and are meant to be installed once at
//! startup; a key the framework already names is refused, so the framework's own glyphs cannot be
//! shadowed.
//!
//! # Data
//!
//! `manifest.tsv` is the framework icon table (the generated [`Icon`] docs repeat it);
//! `assets/svg/` holds the upstream SVGs (MIT, `assets/LICENSE`, pinned commit in
//! `tools/icons/gen.mjs`), and `tools/icons/gen.mjs` turns them into `data/paths.bin` (curves,
//! 1/256-unit i16 coordinates) and `src/generated.rs` (the enum, key table and per-master exact ink
//! bounds). The product build reads only those two files.

// The generated bounds are exact binary fractions of the 1/256-unit grid, printed in full: each
// is exactly an f32, though it has more decimal digits than clippy expects of one.
#[allow(clippy::excessive_precision)]
mod generated;
mod raster;

pub use generated::Icon;

use raster::{Scratch, Transform};
use std::sync::Mutex;

/// The Fluent style an outline comes from.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Variant {
    Regular,
    Filled,
}

/// One row of an icon table.
#[derive(Clone, Copy, Debug)]
pub struct IconEntry {
    pub name: &'static str,
    pub key: char,
    pub key_str: &'static str,
    pub fluent: &'static str,
    pub variant: Variant,
    pub outline: u16,
    /// Where the ink goes, in em units, y down from the em box's top: `[x0, y0, x1, y1]`.
    pub place: [f32; 4],
}

/// Where one outline's masters sit in its set's master table.
#[derive(Clone, Copy, Debug)]
pub struct OutlineEntry {
    pub first_master: u16,
    pub masters: u8,
}

/// One 16/20/24 master of an outline.
#[derive(Clone, Copy, Debug)]
pub struct MasterEntry {
    /// The design grid, in units: the SVG's viewBox side (16, 20 or 24).
    pub design: u8,
    pub verbs: u32,
    pub verb_len: u32,
    /// Index of the master's first point in the blob's coordinate section, in points.
    pub points: u32,
    pub point_len: u32,
    /// Exact ink bounds of the stored geometry, in design units: `[x0, y0, x1, y1]`.
    pub ink: [f32; 4],
}

/// A complete icon table: the entries, the outlines and masters they index, and the outline blob
/// those masters' curves live in. The same four shapes the generated framework table uses, so a
/// product's own table is built exactly like the framework's (see the crate docs).
#[derive(Clone, Copy, Debug)]
pub struct IconSet {
    pub entries: &'static [IconEntry],
    pub outlines: &'static [OutlineEntry],
    pub masters: &'static [MasterEntry],
    /// The set's outline blob: the header and sections `data/paths.bin` documents.
    pub blob: &'static [u8],
}

/// The framework's icon table.
static FRAMEWORK: IconSet =
    IconSet { entries: &generated::ICONS, outlines: &generated::OUTLINES, masters: &generated::MASTERS, blob: BLOB };

/// The tables a product has installed with [`install_icons`], in install order.
static INSTALLED: Mutex<Vec<IconSet>> = Mutex::new(Vec::new());

const BLOB: &[u8] = include_bytes!("../data/paths.bin");
const HEADER_LEN: usize = 16;
const QUANTUM: f32 = 256.0;

fn blob_u32(blob: &[u8], offset: usize) -> usize {
    u32::from_le_bytes([blob[offset], blob[offset + 1], blob[offset + 2], blob[offset + 3]]) as usize
}

fn coordinate_section(blob: &[u8]) -> usize {
    let verbs = blob_u32(blob, 8);
    HEADER_LEN + verbs + verbs % 2
}

/// A resolved icon: one entry of one table, and the table whose outlines, masters and blob the
/// entry means. Copy and host-blind: a glyph carries no engine state.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    set: IconSet,
    index: u16,
}

/// Identity, not geometry: two glyphs are equal when they are the same entry of the same table.
impl PartialEq for Glyph {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && std::ptr::eq(self.set.entries, other.set.entries)
    }
}

impl Eq for Glyph {}

impl Icon {
    /// This framework icon as a resolved glyph.
    pub fn glyph(self) -> Glyph {
        Glyph { set: FRAMEWORK, index: self as u16 }
    }

    /// The manifest id, e.g. `"Dismiss"`.
    pub fn name(self) -> &'static str {
        self.glyph().name()
    }

    /// The character that stands for this icon in text.
    pub fn key(self) -> char {
        self.glyph().key()
    }

    /// [`Icon::key`] as a one-character string.
    pub fn key_str(self) -> &'static str {
        self.glyph().key_str()
    }

    /// The Fluent UI System Icons asset name, e.g. `"Chevron Down"`.
    pub fn fluent_name(self) -> &'static str {
        self.glyph().fluent_name()
    }

    pub fn variant(self) -> Variant {
        self.glyph().variant()
    }

    /// The icon `key` stands for, if any: the framework's table only. A product's installed keys
    /// resolve through [`glyph_for_key`].
    pub fn from_key(key: char) -> Option<Icon> {
        generated::KEYS
            .binary_search_by(|(candidate, _)| candidate.cmp(&key))
            .ok()
            .map(|index| generated::KEYS[index].1)
    }

    /// The design sizes (SVG viewBox sides) this icon's outline ships masters for.
    pub fn master_sizes(self) -> impl Iterator<Item = u8> {
        self.glyph().masters().iter().map(|master| master.design)
    }
}

impl Glyph {
    /// The manifest id, e.g. `"Dismiss"`.
    pub fn name(self) -> &'static str {
        self.entry().name
    }

    /// The character that stands for this glyph in text.
    pub fn key(self) -> char {
        self.entry().key
    }

    /// [`Glyph::key`] as a one-character string.
    pub fn key_str(self) -> &'static str {
        self.entry().key_str
    }

    /// The Fluent UI System Icons asset name, e.g. `"Chevron Down"`.
    pub fn fluent_name(self) -> &'static str {
        self.entry().fluent
    }

    pub fn variant(self) -> Variant {
        self.entry().variant
    }

    /// The design sizes (SVG viewBox sides) this glyph's outline ships masters for.
    pub fn master_sizes(self) -> impl Iterator<Item = u8> {
        self.masters().iter().map(|master| master.design)
    }

    /// Where this glyph lands at em size `size` and `pixels_per_unit` physical pixels per logical
    /// unit, without filling it.
    pub fn metrics(self, size: f32, pixels_per_unit: f32) -> IconMetrics {
        plan(self, size, pixels_per_unit).metrics
    }

    /// Fills this glyph at em size `size` (logical units) and `pixels_per_unit` physical pixels per
    /// logical unit, allocating the tile.
    pub fn rasterize(self, size: f32, pixels_per_unit: f32) -> CoverageTile {
        let mut tile = CoverageTile::default();
        Rasterizer::default().rasterize_glyph_into(self, size, pixels_per_unit, &mut tile);
        tile
    }

    /// [`Glyph::rasterize`] into `tile`, reusing the caller's tile buffer.
    pub fn rasterize_into(self, size: f32, pixels_per_unit: f32, tile: &mut CoverageTile) {
        Rasterizer::default().rasterize_glyph_into(self, size, pixels_per_unit, tile);
    }

    fn entry(self) -> &'static IconEntry {
        &self.set.entries[usize::from(self.index)]
    }

    pub(crate) fn masters(self) -> &'static [MasterEntry] {
        let outline = &self.set.outlines[usize::from(self.entry().outline)];
        let first = usize::from(outline.first_master);
        &self.set.masters[first..first + usize::from(outline.masters)]
    }
}

/// The glyph `key` stands for: the framework's table first, then every table a product installed.
/// This is how the text engine resolves an icon run.
///
/// The framework's own keys are answered without touching the registry, so an application that
/// installs nothing pays exactly what `Icon::from_key` cost before there was a registry.
pub fn glyph_for_key(key: char) -> Option<Glyph> {
    if let Some(icon) = Icon::from_key(key) {
        return Some(icon.glyph());
    }
    installed_glyph(&installed(), key)
}

/// Installs a product's icon table, making every key it names resolve like the framework's.
///
/// Returns `Err(key)` when the table names a key that is already installed — the framework's own
/// keys included — and installs nothing then. Tables are process-wide and installed once at
/// startup.
pub fn install_icons(set: IconSet) -> Result<(), char> {
    let mut installed = installed();
    for entry in set.entries {
        if Icon::from_key(entry.key).is_some() || installed_glyph(&installed, entry.key).is_some() {
            return Err(entry.key);
        }
    }
    installed.push(set);
    Ok(())
}

/// How many tables beyond the framework's are installed.
pub fn installed_icon_sets() -> usize {
    installed().len()
}

/// The installed tables, locked; a panic while they were held does not stop their use (they are
/// only appended to, whole).
fn installed() -> std::sync::MutexGuard<'static, Vec<IconSet>> {
    INSTALLED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The glyph `key` stands for in one of `installed`.
fn installed_glyph(installed: &[IconSet], key: char) -> Option<Glyph> {
    for set in installed {
        if let Some(index) = set.entries.iter().position(|entry| entry.key == key) {
            return Some(Glyph { set: *set, index: index as u16 });
        }
    }
    None
}

/// Where an icon's tile and ink land, in physical pixels relative to the top-left corner of its
/// em box, which the caller places (painter: on the pixel grid).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconMetrics {
    /// The tile's top-left corner.
    pub origin: [i32; 2],
    pub w: u32,
    pub h: u32,
    /// The exact ink extent `[x0, y0, x1, y1]`; the tile is its outward rounding.
    pub ink_bounds: [f32; 4],
    /// The em advance in logical units: the `size` asked for.
    pub advance: f32,
    /// The design size of the master filled.
    pub master: u8,
}

/// One rasterized icon: `w × h` coverage bytes, row-major, 0 = no ink and 255 = full ink.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoverageTile {
    pub w: u32,
    pub h: u32,
    pub a8: Vec<u8>,
    /// The tile's top-left corner in physical pixels from the em box's top-left corner.
    pub origin: [i32; 2],
    /// The exact ink extent `[x0, y0, x1, y1]` in the same pixel space as `origin`.
    pub ink_bounds: [f32; 4],
    /// The em advance in logical units.
    pub advance: f32,
}

struct Plan {
    metrics: IconMetrics,
    master: &'static MasterEntry,
    transform: Transform,
}

fn plan(glyph: Glyph, size: f32, pixels_per_unit: f32) -> Plan {
    let masters = glyph.masters();
    let empty = |master| Plan {
        metrics: IconMetrics { origin: [0, 0], w: 0, h: 0, ink_bounds: [0.0; 4], advance: size.max(0.0), master: 0 },
        master,
        transform: Transform { scale: 0.0, offset: [0.0, 0.0] },
    };
    let em = size * pixels_per_unit;
    if !em.is_finite() || em <= 0.0 || !size.is_finite() {
        return empty(&masters[0]);
    }
    let place = glyph.entry().place;
    let (box_w, box_h) = ((place[2] - place[0]) * em, (place[3] - place[1]) * em);
    // Pixels per design unit that fit `master`'s ink into the placement box.
    let fit = |master: &MasterEntry| {
        let (ink_w, ink_h) = (master.ink[2] - master.ink[0], master.ink[3] - master.ink[1]);
        match (ink_w > 0.0, ink_h > 0.0) {
            (true, true) => (box_w / ink_w).min(box_h / ink_h),
            (true, false) => box_w / ink_w,
            (false, true) => box_h / ink_h,
            (false, false) => 0.0,
        }
    };
    // The master whose design grid is closest to one unit per pixel; ties go to the larger.
    let master = masters
        .iter()
        .min_by(|a, b| {
            let cost = |master: &MasterEntry| fit(master).ln().abs();
            cost(a).total_cmp(&cost(b)).then(b.design.cmp(&a.design))
        })
        .expect("every icon ships at least one master");
    let scale = fit(master);
    if !(scale.is_finite() && scale > 0.0) {
        return empty(master);
    }
    let centre = [(place[0] + place[2]) * 0.5 * em, (place[1] + place[3]) * 0.5 * em];
    let ink_centre = [(master.ink[0] + master.ink[2]) * 0.5, (master.ink[1] + master.ink[3]) * 0.5];
    let half = [(master.ink[2] - master.ink[0]) * 0.5 * scale, (master.ink[3] - master.ink[1]) * 0.5 * scale];
    let ink_bounds = [centre[0] - half[0], centre[1] - half[1], centre[0] + half[0], centre[1] + half[1]];
    let origin = [ink_bounds[0].floor(), ink_bounds[1].floor()];
    let w = (ink_bounds[2].ceil() - origin[0]).max(1.0) as u32;
    let h = (ink_bounds[3].ceil() - origin[1]).max(1.0) as u32;
    Plan {
        metrics: IconMetrics {
            origin: [origin[0] as i32, origin[1] as i32],
            w,
            h,
            ink_bounds,
            advance: size,
            master: master.design,
        },
        master,
        transform: Transform {
            scale,
            offset: [centre[0] - ink_centre[0] * scale - origin[0], centre[1] - ink_centre[1] * scale - origin[1]],
        },
    }
}

/// Where `icon` lands at em size `size` (logical units) and `pixels_per_unit` physical pixels per
/// logical unit, without filling it.
pub fn metrics(icon: Icon, size: f32, pixels_per_unit: f32) -> IconMetrics {
    icon.glyph().metrics(size, pixels_per_unit)
}

/// Fills `icon` at em size `size` (logical units) and `pixels_per_unit` physical pixels per
/// logical unit. Allocates the tile; [`Rasterizer::rasterize_into`] reuses one.
pub fn rasterize(icon: Icon, size: f32, pixels_per_unit: f32) -> CoverageTile {
    icon.glyph().rasterize(size, pixels_per_unit)
}

/// Rasterization with retained working memory: once warm, filling an icon no larger than one
/// filled before into a tile that held one allocates nothing.
#[derive(Default)]
pub struct Rasterizer {
    scratch: Scratch,
}

impl Rasterizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// [`rasterize`] into `tile`, reusing its buffer and this rasterizer's scratch.
    pub fn rasterize_into(&mut self, icon: Icon, size: f32, pixels_per_unit: f32, tile: &mut CoverageTile) {
        self.rasterize_glyph_into(icon.glyph(), size, pixels_per_unit, tile);
    }

    /// [`Glyph::rasterize_into`]: the same fill for a glyph of any installed table.
    pub fn rasterize_glyph_into(&mut self, glyph: Glyph, size: f32, pixels_per_unit: f32, tile: &mut CoverageTile) {
        let plan = plan(glyph, size, pixels_per_unit);
        let metrics = plan.metrics;
        tile.w = metrics.w;
        tile.h = metrics.h;
        tile.origin = metrics.origin;
        tile.ink_bounds = metrics.ink_bounds;
        tile.advance = metrics.advance;
        if metrics.w == 0 || metrics.h == 0 {
            tile.a8.clear();
            return;
        }
        let master = plan.master;
        let blob = glyph.set.blob;
        let verbs = master_verbs(master, blob);
        let mut points = master_points(master, blob);
        raster::fill(&mut self.scratch, verbs, &mut points, plan.transform, metrics.w, metrics.h, &mut tile.a8);
    }
}

/// A master's verbs.
fn master_verbs<'a>(master: &MasterEntry, blob: &'a [u8]) -> &'a [u8] {
    &blob[HEADER_LEN + master.verbs as usize..HEADER_LEN + (master.verbs + master.verb_len) as usize]
}

/// A master's points in design units.
fn master_points<'a>(master: &'a MasterEntry, blob: &'a [u8]) -> impl Iterator<Item = [f32; 2]> + 'a {
    let start = coordinate_section(blob) + master.points as usize * 4;
    blob[start..start + master.point_len as usize * 4].chunks_exact(4).map(|bytes| {
        [
            f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / QUANTUM,
            f32::from(i16::from_le_bytes([bytes[2], bytes[3]])) / QUANTUM,
        ]
    })
}

#[cfg(test)]
mod laws;
