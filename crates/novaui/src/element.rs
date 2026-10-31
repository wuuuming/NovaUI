//! The element tree: slot storage with generation-checked ids, Taffy layout, invalidation bits, per-element
//! display lists in element-local coordinates, promoted layers, damage, and the frame driver.
//!
//! Frame (novaui-spec.md, "Frame"): layout (only if an element needs layout) → positions (only changed subtrees)
//! → paint (re-record PAINT elements, re-place MOVE elements) → raster damaged layer regions
//! (innermost first) → composite damaged canvas regions → present damage.

use crate::anim::{Anim, Ease, Prop, Then};
use crate::geom::*;
use crate::render::primitive::*;
use crate::style::*;
use crate::text::Text;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use taffy::{AvailableSpace, NodeId, TaffyTree};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id {
    idx: u32,
    gen: u32,
}

impl Id {
    /// The id as one number (the accessibility tree's node id): a reused slot is another one.
    pub(crate) fn to_bits(self) -> u64 {
        (self.gen as u64) << 32 | self.idx as u64
    }
}

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod wrapped_translation_tests;

const PAINT: u8 = 1;
const MOVE: u8 = 2;

/// Keep integer pixel widths intact through intrinsic measure → dips → flex layout → pixels.
/// Floating-point cancellation can otherwise turn e.g. 150 px into 149.99998 and make a
/// naturally sized caption ellipsize. Only cover arithmetic roundoff, not a real pixel of room.
fn text_width_px(width: f32, scale: f32) -> i32 {
    let px = width.max(0.0) * scale;
    (px + 4.0 * f32::EPSILON * px.max(1.0)).floor() as i32
}

#[derive(Default, Clone, Copy, Debug)]
pub struct FrameStats {
    pub layout_runs: u64,
    pub positioned: u64,
    pub painted: u64,
    pub moved: u64,
    pub layer_px: u64,
    pub canvas_px: u64,
    pub draws: u64,
    pub prims: u64,

    /// Uploads this frame: atlas px, images, layer (re)allocations.
    pub atlas_px: u64,
    pub images_up: u64,
    pub layer_allocs: u64,
    /// Text: paragraphs laid out (cache misses) and glyphs rasterized this frame.
    pub paras: u64,
    pub glyphs: u64,
    /// Taffy nodes whose layout was recomputed (approx: all when layout ran).
    pub layout_nodes: u64,
    /// CPU time per phase, µs: layout+position, paint, layers+canvas recording, present.
    pub us_layout: u64,
    pub us_paint: u64,
    pub us_draw: u64,
    pub us_present: u64,
}

pub struct El {
    gen: u32,
    alive: bool,
    parent: Option<u32>,
    children: Vec<u32>,
    node: NodeId,
    layout: Layout,
    visual: Visual,
    content: Content,
    tag: u64,
    /// What a text's units are cut from (`.nui` `units`, `timing`, `time`); the doc turns it
    /// into `Visual::units`.
    unit_src: Option<Box<crate::style::UnitSrc>>,
    // interaction state
    hovered: bool,
    focused: bool,
    pressed: bool,
    lit: bool,
    disabled: bool,
    off: bool,
    hover_events: bool,
    press_events: bool,
    wheel_events: bool,
    drag_events: bool,
    editable: bool,
    /// It has the keyboard focus and the focus came from the keyboard (or an assistive client):
    /// the focus visual is drawn around it.
    ring: bool,
    action: Option<crate::app::Action>,
    /// What it is to the keyboard and to assistive clients, where written (`set_semantics`).
    semantics: Option<Box<crate::semantics::Semantics>>,
    promoted: bool,
    translate: (f32, f32),
    scroll: (f32, f32),
    /// An offset group (`set_translate` / `set_scroll` on an element with children): its
    /// descendants keep positions, clips and draw lists as if it had not moved, in a list of
    /// their own, and `goff` is added when they are drawn, damaged and hit. Moving it then costs
    /// one element, not its subtree (`runtime.md`, "Frame cost follows change").
    group: bool,
    // computed
    /// The positioning context this element hands its children.
    kid: Ctx,
    size: (f32, f32),
    abs: Rect,
    wclip: Rect,
    tgt: u32,
    trect: Rect,
    tclip: Rect,
    opacity: f32,
    hidden: bool,
    // promoted: composite in the parent target
    ptgt: u32,
    prect: Rect,
    pclip: Rect,
    popacity: f32,
    /// A layer's margin around the element (px): what its content paints past the element's box
    /// (a hover plate reaching out, a child's shadow) stays in the texture. 0 when it clips.
    lpad: i32,
    comp_painted: Rect,
    /// The innermost offset group above this element (`NONE`: none), across layers: what its
    /// `abs` and `wclip` are relative to.
    grp: u32,
    /// The offset group whose list holds this element's own item, and (promoted) its composite
    /// item (`NONE`: the target's list).
    lgrp: u32,
    cgrp: u32,
    /// A group's content offset (px) in its own list's space, and the clips around its content
    /// there: `gclip` in its list's pixels, `gwclip` in its `grp` space (hit testing).
    goff: (i32, i32),
    gclip: Rect,
    gwclip: Rect,
    /// Where a group's `Item::Group` sits (as `slot` / `slot_list` for its own item).
    gslot: u32,
    gslot_list: u32,
    /// What the last positioning of this element read (its parent's context and its layout box);
    /// `None` before the first. Unchanged, and off every layout-invalidating path, its subtree is
    /// exactly as it was and positioning skips it.
    pkey: Option<PosKey>,
    /// The draw list this element owns (index into `Tree::lists`): the canvas's for the root, its
    /// own for a layer or an offset group; `NONE` otherwise.
    own: u32,
    /// Where the element's own item and (promoted) its composite item sit in their draw lists.
    slot: u32,
    comp_slot: u32,
    slot_list: u32,
    comp_list: u32,
    // display list
    invalid: u8,
    /// Text recording deferred outside the inherited clip. Not runnable work until position
    /// changes; a move/clip change promotes it to PAINT before it can become visible again.
    text_pending: bool,
    /// Its content had an image when it was last recorded (`use_images`).
    draws_images: bool,
    text_margin: i32,
    /// The glyph atlas pages its prims show (`text::page_mask`).
    pages: u64,
    prims: Vec<Prim>,
    local: Rect,
    painted: Rect,
}

/// Reading an element. Every write goes through a `Tree` setter, which knows what the change
/// invalidates (layout, paint, a composite, the bindings that read units).
impl El {
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
    pub fn visual(&self) -> &Visual {
        &self.visual
    }
    pub fn content(&self) -> &Content {
        &self.content
    }
    /// The app's number for the element (`Tree::set_tag`): a row index, a lyric line.
    pub fn tag(&self) -> u64 {
        self.tag
    }
    /// What a text's units are cut from (`Tree::set_unit_src`).
    pub fn unit_src(&self) -> Option<&crate::style::UnitSrc> {
        self.unit_src.as_deref()
    }
    pub fn action(&self) -> Option<&crate::app::Action> {
        self.action.as_ref()
    }
    /// What is written of its semantics (`Tree::set_semantics`).
    pub fn semantics(&self) -> Option<&crate::semantics::Semantics> {
        self.semantics.as_deref()
    }
    /// Its written role, if any.
    pub fn role(&self) -> Option<crate::semantics::Role> {
        self.semantics.as_ref().and_then(|s| s.role)
    }
    pub fn hovered(&self) -> bool {
        self.hovered
    }
    pub fn focused(&self) -> bool {
        self.focused
    }
    pub fn pressed(&self) -> bool {
        self.pressed
    }
    pub fn lit(&self) -> bool {
        self.lit
    }
    pub fn disabled(&self) -> bool {
        self.disabled
    }
    pub fn off(&self) -> bool {
        self.off
    }
    pub fn hover_events(&self) -> bool {
        self.hover_events
    }
    pub fn press_events(&self) -> bool {
        self.press_events
    }
    pub fn wheel_events(&self) -> bool {
        self.wheel_events
    }
    pub fn drag_events(&self) -> bool {
        self.drag_events
    }
    pub fn editable(&self) -> bool {
        self.editable
    }
    pub fn promoted(&self) -> bool {
        self.promoted
    }
    /// The offset `Tree::set_translate` gave it (dips).
    pub fn translate(&self) -> (f32, f32) {
        self.translate
    }
    /// Its children's scroll offset (`Tree::set_scroll`, dips).
    pub fn scroll(&self) -> (f32, f32) {
        self.scroll
    }
}

/// An element's interaction state: each one restyles the element (`Tree::set_state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Hovered,
    Focused,
    Pressed,
    Lit,
    Disabled,
    Off,
}

/// What pointer input an element asks for besides clicks (`.nui` `hover-events` & co.).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listen {
    Hover,
    /// `Event::Pressed` as the left button goes down on it and comes up (or the press is called
    /// off): a look that follows the press with motion of its own (a switch's knob).
    Press,
    Wheel,
    Drag,
}

/// What positioning hands an element from its parent.
#[derive(Clone, Copy, PartialEq)]
struct Ctx {
    x: f32,
    y: f32,
    origin: (i32, i32),
    tgt: u32,
    tclip: Rect,
    wclip: Rect,
    opacity: f32,
    hidden: bool,
}

const NO_CTX: Ctx =
    Ctx { x: 0.0, y: 0.0, origin: (0, 0), tgt: 0, tclip: Rect::EMPTY, wclip: Rect::EMPTY, opacity: 1.0, hidden: false };

#[derive(Clone, Copy, PartialEq)]
struct PosKey {
    c: Ctx,
    lay: (f32, f32, f32, f32),
}

/// No offset group.
const NONE: u32 = u32::MAX;

/// Draw-list items per culling run.
const CHUNK: usize = 32;

/// The margin (dips) a non-clipping layer keeps around its element.
const LAYER_PAD: f32 = 16.0;

struct Layer {
    w: i32,
    h: i32,
    depth: u32,
    damage: Damage,
    allocated: bool,
}

/// A draw list (the canvas's, a layer's, an offset group's content) and, for each run of `CHUNK`
/// items, the bounds of what they paint and whether that must be recomputed: collecting a damaged
/// region skips whole runs it does not touch, so a small damage costs what is near it, not the
/// length of the list.
#[derive(Default)]
struct List {
    items: Vec<Item>,
    chunks: Vec<(Rect, bool)>,
}

#[derive(Clone, Copy)]
enum Item {
    El(u32),
    Layer(u32),
    /// An offset group's content: its own list, drawn moved by its `goff`.
    Group(u32),
}

/// How long a window size must hold before the backend gives back what a larger size used.
const TRIM_AFTER: f64 = 0.5;

/// How long an image keeps its texture while no shown element draws it (every element drawing
/// it hidden or gone, its handle kept elsewhere: an app's cache, a hidden page).
const IMAGE_KEEP: f64 = 10.0;
/// A deadline the tree serves itself, on the host clock: when it is (none: not set), and
/// whether that time has come (`serve`); its work takes it (`take_due`).
#[derive(Clone, Copy, Debug, Default)]
struct Deadline {
    at: Option<f64>,
    due: bool,
}

impl Deadline {
    /// Set to `at` (or cleared), not due.
    fn set(&mut self, at: Option<f64>) {
        self.at = at;
        self.due = false;
    }
    /// When it is, while it has not come.
    fn pending(&self) -> Option<f64> {
        self.at.filter(|_| !self.due)
    }
    /// It is due once `now` reached it.
    fn serve(&mut self, now: f64) {
        if self.at.is_some_and(|t| now >= t) {
            self.due = true;
        }
    }
    /// Whether it was due; no longer.
    fn take_due(&mut self) -> bool {
        std::mem::take(&mut self.due)
    }
}

/// An image some element drew, and who draws it.
struct ImageUse {
    /// Weak: the handles in element content (and the app's own) keep an image; the first frame
    /// after the last one goes frees its texture.
    weak: crate::image::WeakImage,
    /// Elements that recorded it; checked against their content when the image is looked at.
    users: Vec<u32>,
    /// Since when no shown element draws it (host clock).
    unshown_since: Option<f64>,
}

pub struct Tree {
    els: Vec<El>,
    /// What a dead id reads (`el`): an element with nothing in it, belonging nowhere.
    dead: El,
    free: Vec<u32>,
    taffy: TaffyTree<u32>,
    root: Id,
    /// Physical pixels per dip.
    scale: f32,
    w: i32,
    h: i32,
    clear: Color,
    /// A 1-px line around the window, over everything (a host's window edge where the system
    /// draws none: Windows 10, X11). None while maximized or fullscreen.
    frame_stroke: Option<Color>,
    text: Text,
    layers: BTreeMap<u32, Layer>,
    /// The draw lists, by index (`El::own`, `slot_list`): the first `n_lists` are the current
    /// ones; the rest keep their capacity for the next rebuild.
    lists: Vec<List>,
    n_lists: usize,
    order: Vec<u32>,
    structure: bool,
    layout_dirty: bool,
    pos_dirty: Vec<u32>,
    /// Elements whose layout inputs changed since the last layout (style, content measure,
    /// children); `touch_all` when every element's did (a rescale).
    touched: Vec<u32>,
    touch_all: bool,
    /// During a post-layout positioning: elements on a changed path (empty: walk everything).
    on_path: Vec<u32>,
    path_gen: u32,
    walk_all: bool,
    /// Elements with a material (rebuilt with the draw lists): a frame widens damage around
    /// these only, instead of scanning every element.
    materials: Vec<u32>,
    changed: Vec<u32>,
    /// The elements this frame's paint handled (the damage audit skips them).
    audit_fresh: Vec<u32>,
    damage: Damage,
    resized: bool,
    /// When a settled resize trims the backend (host clock), and whether that time has come.
    trim: Deadline,
    /// The window needs its retained pixels presented again, not re-rasterized.
    exposed: bool,
    freed_layers: Vec<u32>,
    /// Images some element drew, by texture id: the ones with a texture on the backend. A
    /// texture goes the frame after its image's last handle does, or `IMAGE_KEEP` after the
    /// last shown element drawing it was hidden or removed (`look_at_images`); an element that
    /// records it again uploads it again. A map, not a slot vector: image ids are process-wide
    /// and never reused (a texture is freed a frame after its image goes, so a reused id could
    /// name two textures at once).
    images: HashMap<u32, ImageUse>,
    image_uploads: Vec<u32>,
    /// An element drawing an image was recorded again or removed since the images were last
    /// looked at.
    images_moved: bool,
    /// When an image no shown element draws may lose its texture (host clock; served by the
    /// first turn after it, never a wake of its own), and whether that time has come.
    image_free: Deadline,
    /// The host's wake, for a backend that makes an image's texture off the UI thread
    /// (`Backend::image_make`); `None` (no host) makes them at once.
    pub(crate) image_wake: Option<crate::render::primitive::Wake>,
    /// Scratch for `Backend::images_made`.
    images_made: Vec<u32>,
    /// Bumped by every `remove`: `Ui` drops its state for dead elements when it moved.
    pub(crate) removal_version: u64,
    /// Bumped when an element's `unit_src` changes.
    pub(crate) unit_version: u64,
    anims: Vec<Anim>,
    /// Elements on their way out (`exit(.., Then::Hide)`) and when they go.
    leaving: Vec<(Id, f64)>,
    pub(crate) stats: FrameStats,
    last: FrameStats,
    scratch: Vec<Draw>,
    /// Layers, innermost first (rebuilt with the draw lists).
    layer_order: Vec<u32>,
    layer_rects: Vec<Rect>,
    /// Around each material, what it samples (`widen_for_materials`).
    reach: Vec<Rect>,
    /// `image::DROP_VERSION` when the registry was last checked for images that went.
    seen_drop_version: u64,
    /// Some shown layer may hold damage (set by damaging a layer, cleared by a frame).
    layer_work: bool,
    /// What the last frame drew (window pixels); kept, not allocated each frame.
    pub(crate) drawn: Vec<Rect>,
    /// Bumped by every layout and structure change: where elements are may have changed
    /// without anything being drawn under the pointer.
    pub(crate) geometry_version: u64,
    /// Positioning's walk (kept, not allocated each frame).
    pos_stack: Vec<(u32, Ctx)>,
    /// A subtree walk's stack (`kill`, `subtree_paint`, `set_disabled`, `make_group`; kept).
    walk: Vec<u32>,
    /// The elements whose accessibility node may have changed since the last look, while an
    /// assistive client has the tree (`access`); `None` (and nothing kept) until one asks.
    access: Option<Vec<u32>>,
    /// `rebuild_lists`' walk and the layers it met with their depths (kept).
    build_stack: Vec<(u32, u32, u32, u32, u32)>,
    seen_layers: Vec<(u32, u32)>,
    /// Layouts run since the tree was made (`NOVAUI_TRACE`: layouts per turn).
    pub(crate) layout_version: u64,
    /// Hit tests asked, and elements they visited, since the tree was made.
    pub(crate) hit_tests: std::cell::Cell<u64>,
    pub(crate) hit_visits: std::cell::Cell<u64>,
    /// Labels and damage reasons (debug builds: the inspector's).
    #[cfg(any(debug_assertions, test))]
    dev: crate::dev::TreeDev,
}

/// Taffy reports only node ids it does not know. The tree makes and frees every node itself, so
/// an error is a bug here: it fails debug builds and is skipped in release.
#[track_caller]
fn taffy_ok<T>(r: taffy::TaffyResult<T>) {
    debug_assert!(r.is_ok(), "taffy: {:?}", r.err());
}

/// An element in a contract's message: its content and its tag.
fn what(e: &El) -> String {
    let content = match &e.content {
        Content::None => "box".to_string(),
        Content::Text(t) => format!("text {:?}", t.text),
        Content::Icon(_) => "icon".to_string(),
        Content::Path(_) => "path".to_string(),
        Content::Image(_) | Content::ImageCrossfade(_) => "image".to_string(),
        Content::Effect(..) => "effect".to_string(),
    };
    format!("{content}, tag {}", e.tag)
}

/// A visual's plain-data fields, by value (its units left out: `None`).
fn plain(v: &Visual) -> Visual {
    Visual { units: None, ..*v }
}

fn px(v: f32, s: f32) -> i32 {
    (v * s).round() as i32
}

/// The keyboard focus visual around `r` (WinUI's: a 2 dip ring in `focus-stroke-outer` whose
/// outer edge stands 3 dips out, and a 1 dip ring in `focus-stroke-inner` inside it), its corners
/// following the element's `radius` (px).
fn focus_visual(r: Rect, radius: f32, s: f32, out: &mut Vec<Prim>) {
    let outer = crate::theme::color("focus-stroke-outer").unwrap_or(argb(0xFFFFFFFF));
    let inner = crate::theme::color("focus-stroke-inner").unwrap_or(argb(0xB3000000));
    let (out_at, thick, thin) = (px(3.0, s).max(2), px(2.0, s).max(1), px(1.0, s).max(1));
    for (at, th, c) in [(out_at, thick, outer), (out_at - thick, thin, inner)] {
        if c[3] > 0.0 {
            out.push(Prim::Quad {
                rect: r.inflate(at),
                radius: if radius > 0.0 { radius + at as f32 } else { 0.0 },
                square: 0,
                fill: TRANSPARENT,
                border: th as f32,
                border_color: c,
            });
        }
    }
}

fn plate_rect(plate: Option<[f32; 4]>, box_rect: Rect, scale: f32) -> Rect {
    plate.map_or(box_rect, |[x, y, w, h]| {
        Rect::new(
            box_rect.x + px(x, scale),
            box_rect.y + px(y, scale),
            px(x + w, scale) - px(x, scale),
            px(y + h, scale) - px(y, scale),
        )
    })
}

impl Tree {
    /// The root element (the window's content box).
    pub fn root(&self) -> Id {
        self.root
    }

    /// Physical pixels per dip.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The window's size in physical pixels (`resize`).
    pub fn window_size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    /// What the window shows where no element paints.
    pub fn clear(&self) -> Color {
        self.clear
    }

    /// Sets the clear colour; the whole window is drawn again when it changed.
    pub fn set_clear(&mut self, c: Color) {
        if self.clear != c {
            self.clear = c;
            self.damage_window();
        }
    }

    /// Text shaping and measuring (its caches are the tree's). The fallback font is the
    /// tree's to change (`set_font`): every text is measured again with it.
    pub fn text(&self) -> &Text {
        &self.text
    }
    pub fn text_mut(&mut self) -> &mut Text {
        &mut self.text
    }

    /// What the last frame did (`FrameStats`).
    pub fn last(&self) -> FrameStats {
        self.last
    }

    /// Sizes of the growable tables (leak checks): element slots, live elements, free slots,
    /// layers, draw lists, images, animations, taffy nodes.
    pub fn sizes(&self) -> [usize; 8] {
        [
            self.els.len(),
            self.els.iter().filter(|e| e.alive).count(),
            self.free.len(),
            self.layers.len(),
            self.lists[..self.n_lists].iter().map(|l| l.items.len()).sum(),
            self.images.len(),
            self.anims.len(),
            self.taffy.total_node_count(),
        ]
    }
    pub fn new(w: i32, h: i32, scale: f32) -> Self {
        let mut taffy = TaffyTree::new();
        taffy.disable_rounding();
        let mut t = Tree {
            els: Vec::new(),
            dead: El {
                alive: false,
                ..El::blank(NodeId::from(u64::MAX), Layout::default(), Visual::default(), Content::None)
            },
            free: Vec::new(),
            taffy,
            root: Id { idx: 0, gen: 0 },
            scale,
            w,
            h,
            clear: rgb(0, 0, 0),
            frame_stroke: None,
            text: Text::new(),
            layers: BTreeMap::new(),
            lists: Vec::new(),
            n_lists: 0,
            order: Vec::new(),
            structure: true,
            layout_dirty: true,
            pos_dirty: Vec::new(),
            touched: Vec::new(),
            touch_all: true,
            on_path: Vec::new(),
            path_gen: 0,
            walk_all: false,
            materials: Vec::new(),
            changed: Vec::new(),
            audit_fresh: Vec::new(),
            damage: Damage::default(),
            resized: true,
            trim: Deadline::default(),
            exposed: false,
            freed_layers: Vec::new(),
            images: HashMap::new(),
            image_uploads: Vec::new(),
            images_moved: false,
            image_free: Deadline::default(),
            image_wake: None,
            images_made: Vec::new(),
            removal_version: 0,
            unit_version: 0,
            anims: Vec::new(),
            leaving: Vec::new(),
            stats: FrameStats::default(),
            last: FrameStats::default(),
            scratch: Vec::new(),
            pos_stack: Vec::new(),
            walk: Vec::new(),
            access: None,
            build_stack: Vec::new(),
            seen_layers: Vec::new(),
            drawn: Vec::new(),
            seen_drop_version: u64::MAX,
            layer_order: Vec::new(),
            layer_rects: Vec::new(),
            reach: Vec::new(),
            layer_work: false,
            geometry_version: 0,
            layout_version: 0,
            hit_tests: std::cell::Cell::new(0),
            hit_visits: std::cell::Cell::new(0),
            #[cfg(any(debug_assertions, test))]
            dev: Default::default(),
        };
        let root = t.alloc(
            Layout { width: Len::Pct(100.0), height: Len::Pct(100.0), ..Default::default() },
            Visual::default(),
            Content::None,
        );
        t.root = root;
        // A new target has no valid pixels, even where no element paints. Establish the
        // clear-color invariant before incremental damage can expose a previously unused area.
        t.damage.add(Rect::new(0, 0, w, h));
        t
    }

    // ---- structure ------------------------------------------------------------------------

    fn alloc(&mut self, layout: Layout, visual: Visual, content: Content) -> Id {
        let node = self.taffy.new_leaf(layout.to_taffy()).expect("taffy");
        let el = El::blank(node, layout, visual, content);
        let idx = match self.free.pop() {
            Some(i) => {
                let gen = self.els[i as usize].gen + 1;
                let mut prims = std::mem::take(&mut self.els[i as usize].prims);
                prims.clear();
                self.els[i as usize] = El { gen, prims, ..el };
                i
            }
            None => {
                self.els.push(el);
                (self.els.len() - 1) as u32
            }
        };
        taffy_ok(self.taffy.set_node_context(node, Some(idx)));
        self.changed.push(idx);
        self.access_touch(idx);
        Id { idx, gen: self.els[idx as usize].gen }
    }

    pub fn alive(&self, id: Id) -> bool {
        self.slot(id).is_some()
    }

    /// The slot of a live element: the one checked way from an `Id` to its element. A dead id
    /// (removed, its slot maybe reused since) has none, so every public entry reads it as empty
    /// and changes nothing.
    #[inline]
    fn slot(&self, id: Id) -> Option<u32> {
        self.els.get(id.idx as usize).is_some_and(|e| e.alive && e.gen == id.gen).then_some(id.idx)
    }

    pub(crate) fn id_of(&self, idx: u32) -> Id {
        Id { idx, gen: self.els[idx as usize].gen }
    }

    /// The live element `Id::to_bits` gave `bits`, if it is still alive.
    pub(crate) fn id_from_bits(&self, bits: u64) -> Option<Id> {
        let id = Id { idx: bits as u32, gen: (bits >> 32) as u32 };
        self.alive(id).then_some(id)
    }

    // ---- accessibility --------------------------------------------------------------------

    /// Starts (or stops) keeping the elements whose accessibility node may change (`access`).
    pub(crate) fn access_track(&mut self, on: bool) {
        self.access = on.then(Vec::new);
    }

    /// Element `i` may look different to an assistive client: what it shows, where, its
    /// children, what it does.
    #[inline]
    fn access_touch(&mut self, i: u32) {
        if let Some(t) = &mut self.access {
            t.push(i);
        }
    }

    /// The live elements touched since the last call, once each.
    pub(crate) fn access_touched(&mut self) -> Vec<Id> {
        let Some(t) = &mut self.access else { return Vec::new() };
        let mut t = std::mem::take(t);
        t.sort_unstable();
        t.dedup();
        t.into_iter().map(|i| self.id_of(i)).filter(|&id| self.alive(id)).collect()
    }

    /// An offset group: its content moves by its offset (`access_box`).
    pub(crate) fn access_group(&self, id: Id) -> bool {
        self.slot(id).is_some_and(|i| self.els[i as usize].group && !self.els[i as usize].promoted)
    }

    /// Its box (px) in its offset group's content space, and, for a group, the offset its own
    /// content is moved by (`win_off`).
    pub(crate) fn access_box(&self, id: Id) -> (Rect, Option<(i32, i32)>) {
        let Some(i) = self.slot(id) else { return (Rect::EMPTY, None) };
        let e = &self.els[i as usize];
        (e.abs, self.access_group(id).then_some(e.goff))
    }

    /// The element's parent; `None` for the root and for a dead id.
    pub fn parent(&self, id: Id) -> Option<Id> {
        let i = self.slot(id)?;
        self.els[i as usize].parent.map(|p| self.id_of(p))
    }

    /// The element; a dead id reads as an empty one (default layout, visual and content, no
    /// children, no parent).
    pub fn el(&self, id: Id) -> &El {
        match self.slot(id) {
            Some(i) => &self.els[i as usize],
            None => &self.dead,
        }
    }

    /// Appends a new element as the last child of `parent`. Under a dead parent nothing is
    /// made: the id returned is dead.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn add(&mut self, parent: Id, layout: Layout, visual: Visual, content: Content) -> Id {
        self.insert_at(parent, None, layout, visual, content)
    }

    /// Inserts a new element as child `at` of `parent` (`add`'s rules).
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn insert(&mut self, parent: Id, at: usize, layout: Layout, visual: Visual, content: Content) -> Id {
        self.insert_at(parent, Some(at), layout, visual, content)
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    fn insert_at(&mut self, parent: Id, at: Option<usize>, layout: Layout, visual: Visual, content: Content) -> Id {
        let Some(p) = self.slot(parent) else {
            return Id { idx: u32::MAX, gen: 0 };
        };
        let id = self.alloc(layout, visual, content);
        self.attach(p, id, at);
        self.why(id.idx, "made");
        id
    }

    fn attach(&mut self, p: u32, id: Id, at: Option<usize>) {
        self.els[id.idx as usize].parent = Some(p);
        let at = at.unwrap_or(self.els[p as usize].children.len()).min(self.els[p as usize].children.len());
        self.els[p as usize].children.insert(at, id.idx);
        let (pn, n) = (self.els[p as usize].node, self.els[id.idx as usize].node);
        taffy_ok(self.taffy.insert_child_at_index(pn, at, n));
        self.children_changed(p);
    }

    /// Removes `id` and its subtree; their pixels are damaged.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn remove(&mut self, id: Id) {
        if !self.alive(id) || id == self.root {
            return;
        }
        let idx = id.idx;
        if let Some(p) = self.els[idx as usize].parent {
            self.why(p, "child removed");
            self.els[p as usize].children.retain(|&c| c != idx);
            let (pn, n) = (self.els[p as usize].node, self.els[idx as usize].node);
            taffy_ok(self.taffy.remove_child(pn, n));
            self.touched.push(p);
            self.access_touch(p);
        }
        self.kill(idx);
    }

    /// Frees the detached subtree at `idx`: its elements die, their pixels are damaged.
    fn kill(&mut self, idx: u32) {
        let mut stack = std::mem::take(&mut self.walk);
        stack.clear();
        stack.push(idx);
        while let Some(i) = stack.pop() {
            stack.extend(self.els[i as usize].children.iter().copied());
            let e = &mut self.els[i as usize];
            e.alive = false;
            e.parent = None;
            e.children.clear();
            e.action = None;
            e.semantics = None;
            // Content may hold image handles: a dead element keeps none alive.
            e.content = Content::None;
            self.images_moved |= std::mem::take(&mut e.draws_images);
            let pages = std::mem::take(&mut e.pages);
            let (promoted, node) = (e.promoted, e.node);
            self.text.repage(pages, 0);
            taffy_ok(self.taffy.remove(node));
            self.damage_own(i);
            if promoted {
                self.damage_comp(i);
                self.free_layer(i);
            }
            self.free.push(i);
        }
        self.walk = stack;
        let els = &self.els;
        self.anims.retain(|a| els[a.el as usize].alive);
        self.leaving.retain(|(l, _)| els[l.idx as usize].alive && els[l.idx as usize].gen == l.gen);
        self.removal_version += 1;
        self.structure = true;
        self.layout_dirty = true;
    }

    /// Removes all children of `id`.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn clear_children(&mut self, id: Id) {
        if !self.alive(id) || self.els[id.idx as usize].children.is_empty() {
            return;
        }
        let p = id.idx;
        self.why(p, "children removed");
        let mut kids = std::mem::take(&mut self.els[p as usize].children);
        taffy_ok(self.taffy.set_children(self.els[p as usize].node, &[]));
        self.touched.push(p);
        self.access_touch(p);
        for &k in &kids {
            self.kill(k);
        }
        // The parent keeps the vector's room for the children that come next.
        kids.clear();
        self.els[p as usize].children = kids;
    }

    pub fn children(&self, id: Id) -> Vec<Id> {
        self.el(id).children.iter().map(|&c| self.id_of(c)).collect()
    }

    /// Moves child `id` to position `at` among its siblings.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn reorder(&mut self, id: Id, at: usize) {
        let Some(i) = self.slot(id) else { return };
        let Some(p) = self.els[i as usize].parent else { return };
        self.why(p, "children reordered");
        let ch = &mut self.els[p as usize].children;
        ch.retain(|&c| c != i);
        let at = at.min(ch.len());
        ch.insert(at, i);
        let (pn, n) = (self.els[p as usize].node, self.els[i as usize].node);
        taffy_ok(self.taffy.remove_child(pn, n));
        taffy_ok(self.taffy.insert_child_at_index(pn, at, n));
        self.children_changed(p);
        self.subtree_paint(p);
    }

    // ---- setters --------------------------------------------------------------------------

    /// Notes why element `i` changed, and who asked (debug builds: the inspector shows it).
    #[cfg(any(debug_assertions, test))]
    #[track_caller]
    fn why(&mut self, i: u32, what: &'static str) {
        let gen = self.els[i as usize].gen;
        self.dev.why(i, gen, what, std::panic::Location::caller());
    }
    #[cfg(not(any(debug_assertions, test)))]
    #[inline(always)]
    fn why(&mut self, _: u32, _: &'static str) {}

    /// What element `id` is, as its builder named it (a `.nui` element's kind, name and line).
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn set_dev_label(&mut self, id: Id, label: String) {
        if let Some(i) = self.slot(id) {
            self.dev.label(i, id.gen, label);
        }
    }
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn dev_label(&self, id: Id) -> Option<&str> {
        self.slot(id).and_then(|i| self.dev.label_of(i, id.gen))
    }
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn dev_why(&self, id: Id) -> Option<crate::dev::Why> {
        self.slot(id).and_then(|i| self.dev.why_of(i, id.gen))
    }

    /// Moves `id` (with its subtree) under `parent` at `at` (a `.nui` reload's new elements,
    /// built aside, put in place). Its pixels are damaged where it was and painted where it goes.
    #[cfg(any(debug_assertions, test))]
    pub(crate) fn dev_move(&mut self, id: Id, parent: Id, at: usize) {
        let (Some(i), Some(p)) = (self.slot(id), self.slot(parent)) else { return };
        if let Some(old) = self.els[i as usize].parent {
            self.els[old as usize].children.retain(|&c| c != i);
            let (on, n) = (self.els[old as usize].node, self.els[i as usize].node);
            taffy_ok(self.taffy.remove_child(on, n));
            self.touched.push(old);
        }
        self.damage_own(i);
        // Positioned afresh where it lands.
        self.els[i as usize].pkey = None;
        self.attach(p, id, Some(at));
        self.subtree_paint(i);
    }

    fn flag(&mut self, i: u32, f: u8) {
        let e = &mut self.els[i as usize];
        if e.invalid == 0 {
            self.changed.push(i);
        }
        e.invalid |= f;
        self.access_touch(i);
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn mark(&mut self, id: Id) {
        if self.alive(id) {
            self.why(id.idx, "marked");
            self.flag(id.idx, PAINT);
        }
    }

    fn subtree_paint(&mut self, i: u32) {
        let mut stack = std::mem::take(&mut self.walk);
        stack.clear();
        stack.push(i);
        while let Some(i) = stack.pop() {
            self.flag(i, PAINT);
            stack.extend(self.els[i as usize].children.iter().copied());
        }
        self.walk = stack;
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_layout(&mut self, id: Id, f: impl FnOnce(&mut Layout)) {
        if !self.alive(id) {
            return;
        }
        let e = &mut self.els[id.idx as usize];
        let old = e.layout;
        f(&mut e.layout);
        if e.layout != old {
            let st = e.layout.to_taffy();
            let node = e.node;
            taffy_ok(self.taffy.set_style(node, st));
            self.touch(id.idx);
            if (old.display == Display::None) != (self.els[id.idx as usize].layout.display == Display::None) {
                self.subtree_paint(id.idx);
            } else if old.padding != self.els[id.idx as usize].layout.padding {
                // Text is placed inside the padding when it is recorded; a box whose size the
                // layout fixes would otherwise keep its text where the old padding put it.
                self.flag(id.idx, PAINT);
            }
            self.why(id.idx, "layout");
        }
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_visual(&mut self, id: Id, f: impl FnOnce(&mut Visual)) {
        if !self.alive(id) {
            return;
        }
        let e = &mut self.els[id.idx as usize];
        // Every field but the units is plain data: kept by value. The units are compared by
        // pointer first (the same `Rc` is the same units), by value only when they were replaced.
        let old = Visual { units: None, ..plain(&e.visual) };
        let old_units = e.visual.units.clone();
        f(&mut e.visual);
        let units_same = match (&old_units, &e.visual.units) {
            (Some(a), Some(b)) => std::rc::Rc::ptr_eq(a, b) || a == b,
            (a, b) => a.is_none() && b.is_none(),
        };
        let now = plain(&e.visual);
        let rest_same = Visual { opacity: old.opacity, units: None, ..now } == old;
        if !(units_same && rest_same && e.visual.opacity == old.opacity) {
            let v = &now;
            // Opacity and visibility reach descendants; clip too.
            let subtree = v.opacity != old.opacity || v.visible != old.visible || v.clip != old.clip;
            let promoted = e.promoted;
            self.why(id.idx, "visual");
            let composite_changed =
                promoted && (v.plate != old.plate || v.shadow != old.shadow || v.material != old.material);
            if v.material.is_some() != old.material.is_some() {
                self.structure = true;
            }
            if subtree {
                self.pos_dirty.push(id.idx);
            }
            // A layer's opacity is applied when it is composited: its content is unchanged.
            let only_opacity = promoted && units_same && rest_same;
            if !only_opacity {
                self.flag(id.idx, PAINT);
            }
            if composite_changed {
                self.damage_comp(id.idx);
                self.update_comp(id.idx);
            }
        }
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_content(&mut self, id: Id, c: Content) {
        if !self.alive(id) {
            return;
        }
        let e = &mut self.els[id.idx as usize];
        if e.content != c {
            if e.unit_src.is_some() {
                self.unit_version += 1;
            }
            let relayout = match (&e.content, &c) {
                (Content::Text(a), Content::Text(b)) => {
                    a.text != b.text
                        || a.size != b.size
                        || a.line != b.line
                        || a.weight != b.weight
                        || a.lines != b.lines
                        || a.wrap != b.wrap
                }
                (Content::Icon(a), Content::Icon(b)) => a.size != b.size,
                (Content::Path(a), Content::Path(b)) => a.size != b.size,
                (a, b) => std::mem::discriminant(a) != std::mem::discriminant(b),
            };
            e.content = c;
            self.why(id.idx, "content");
            self.content_changed(id.idx, relayout);
        }
    }

    /// Element `i`'s content changed (`relayout`: in what it measures): it is painted again, and
    /// measured again unless the layout fixes its size.
    fn content_changed(&mut self, i: u32, relayout: bool) {
        let e = &self.els[i as usize];
        // A box whose size the layout fixes (both axes) measures nothing: new text in it
        // re-records it, it moves nothing.
        if relayout && !e.layout.fixed_size() {
            let node = e.node;
            taffy_ok(self.taffy.mark_dirty(node));
            self.touch(i);
        }
        self.flag(i, PAINT);
    }

    /// Sets the text of a text element (keeps its style): written into the string it has.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_text(&mut self, id: Id, s: &str) {
        if !self.alive(id) {
            return;
        }
        let e = &mut self.els[id.idx as usize];
        let Content::Text(t) = &mut e.content else { return };
        if t.text == s {
            return;
        }
        t.text.clear();
        t.text.push_str(s);
        if e.unit_src.is_some() {
            self.unit_version += 1;
        }
        self.why(id.idx, "text");
        self.content_changed(id.idx, true);
    }

    /// Changes what a text's units are cut from (`El::unit_src`; made when first set).
    pub fn set_unit_src(&mut self, id: Id, f: impl FnOnce(&mut crate::style::UnitSrc)) {
        if !self.alive(id) {
            return;
        }
        let src = self.els[id.idx as usize].unit_src.get_or_insert_with(Default::default);
        // The timing is compared by pointer first (the same `Rc` is the same timing).
        let (words, time, timing) = (src.words, src.time, src.timing.clone());
        f(src);
        let same = src.words == words
            && src.time == time
            && (std::rc::Rc::ptr_eq(&src.timing, &timing) || src.timing == timing);
        if !same {
            self.unit_version += 1;
        }
    }

    /// How many lines element `id`'s text takes at `width` dips, broken as its layout breaks it
    /// (its size, line, weight, wrap and line limit): what a rule that grows for a second line
    /// reads before the element stands at that width. 0 without text. From the text cache: no
    /// atlas, no layout of the tree.
    pub fn text_lines(&mut self, id: Id, width: f32) -> usize {
        let Some(i) = self.slot(id) else { return 0 };
        let Content::Text(t) = &self.els[i as usize].content else { return 0 };
        let s = self.scale;
        let wpx = if t.wrap { text_width_px(width, s) } else { 0 };
        self.text.measure(&t.text, &t.style(s), wpx, t.max_lines()).lines
    }

    /// A text's line height, in dips.
    pub fn text_line(&self, id: Id) -> Option<f32> {
        match &self.el(id).content {
            Content::Text(t) => Some(t.line),
            _ => None,
        }
    }

    pub fn text_of(&self, id: Id) -> Option<&str> {
        match &self.el(id).content {
            Content::Text(t) => Some(&t.text),
            _ => None,
        }
    }

    /// Sets one interaction state; the element is painted again when it changed.
    ///
    /// Input owns `Hovered`, `Pressed` and `Focused` (`Ui`'s pointer and focus): set directly,
    /// they last until the next pointer move or focus change (tests, custom hosts). This sets
    /// the one element; `set_disabled` disables a subtree.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_state(&mut self, id: Id, state: State, on: bool) {
        if !self.alive(id) {
            return;
        }
        let e = &mut self.els[id.idx as usize];
        let bit = match state {
            State::Hovered => &mut e.hovered,
            State::Focused => &mut e.focused,
            State::Pressed => &mut e.pressed,
            State::Lit => &mut e.lit,
            State::Disabled => &mut e.disabled,
            State::Off => &mut e.off,
        };
        if *bit != on {
            *bit = on;
            self.why(id.idx, "state");
            self.flag(id.idx, PAINT);
        }
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_lit(&mut self, id: Id, on: bool) {
        self.set_state(id, State::Lit, on);
    }
    /// Disabled holds for the whole subtree (WinUI's inherited `IsEnabled`): a row's icon and
    /// label take their `fg-disabled` with it, and nothing inside it can be pressed.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_disabled(&mut self, id: Id, on: bool) {
        if !self.alive(id) {
            return;
        }
        let mut stack = std::mem::take(&mut self.walk);
        stack.clear();
        stack.push(id.idx);
        while let Some(i) = stack.pop() {
            let e = &mut self.els[i as usize];
            if e.disabled != on {
                e.disabled = on;
                self.why(i, "disabled");
                self.flag(i, PAINT);
            }
            stack.extend(self.els[i as usize].children.iter().copied());
        }
        self.walk = stack;
    }
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_off(&mut self, id: Id, on: bool) {
        self.set_state(id, State::Off, on);
    }

    /// A text field's element (`Ui::text_field`): a hit target that takes the focus.
    pub(crate) fn set_editable(&mut self, id: Id) {
        if self.alive(id) {
            self.els[id.idx as usize].editable = true;
            self.access_touch(id.idx);
        }
    }

    /// Moves an element (dips) without layout. An element with children becomes an offset group
    /// (`group`), moved in whole pixels: its subtree is not walked again.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_translate(&mut self, id: Id, dx: f32, dy: f32) {
        if self.alive(id) && self.els[id.idx as usize].translate != (dx, dy) {
            self.els[id.idx as usize].translate = (dx, dy);
            self.why(id.idx, "translate");
            self.make_group(id.idx);
            self.pos_dirty.push(id.idx);
        }
    }

    /// Scrolls an element's children (dips). The element becomes an offset group.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_scroll(&mut self, id: Id, sx: f32, sy: f32) {
        if self.alive(id) && self.els[id.idx as usize].scroll != (sx, sy) {
            self.els[id.idx as usize].scroll = (sx, sy);
            self.why(id.idx, "scroll");
            self.make_group(id.idx);
            // Positioned again from itself, whether a group (its offset) or a layer (no group:
            // its children's context carries the scroll). Its children alone would be laid from
            // the context it gave them before the scroll, and stand where they stood.
            self.pos_dirty.push(id.idx);
        }
    }

    /// Makes `i` an offset group, once: what its subtree drew is damaged where it is now (its
    /// items move to a list of their own, in other coordinates).
    fn make_group(&mut self, i: u32) {
        let e = &self.els[i as usize];
        if e.group || e.promoted || e.children.is_empty() || i == self.root.idx {
            return;
        }
        self.damage_under(i);
        self.els[i as usize].group = true;
        self.structure = true;
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_opacity(&mut self, id: Id, o: f32) {
        self.set_visual(id, |v| v.opacity = o);
    }

    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_visible(&mut self, id: Id, on: bool) {
        self.set_visual(id, |v| v.visible = on);
    }

    /// Promotes an element to its own layer (texture): changes of its translation and opacity
    /// then cost only a composite, and changes inside it re-raster only the layer.
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_promoted(&mut self, id: Id, on: bool) {
        if !self.alive(id) || self.els[id.idx as usize].promoted == on {
            return;
        }
        self.why(id.idx, "promoted");
        self.els[id.idx as usize].promoted = on;
        self.damage_own(id.idx);
        if on {
            // What is under it leaves the target it was drawn in for the layer, cut at the
            // layer's edge: what showed past that edge is gone.
            self.damage_under(id.idx);
        } else {
            self.damage_comp(id.idx);
            self.free_layer(id.idx);
        }
        self.structure = true;
        self.pos_dirty.push(id.idx);
        self.subtree_paint(id.idx);
    }

    pub fn set_action(&mut self, id: Id, a: Option<&str>) {
        if self.alive(id) {
            self.els[id.idx as usize].action = a.map(crate::app::Action::parse);
            self.access_touch(id.idx);
        }
    }

    /// Writes what the element is to the keyboard and to assistive clients (`Semantics`).
    pub fn set_semantics(&mut self, id: Id, f: impl FnOnce(&mut crate::semantics::Semantics)) {
        let Some(i) = self.slot(id) else { return };
        let e = &mut self.els[i as usize];
        let old = e.semantics.take();
        let mut s = old.as_deref().cloned().unwrap_or_default();
        f(&mut s);
        let changed = s != old.as_deref().cloned().unwrap_or_default();
        e.semantics = match (s == Default::default(), old) {
            (true, _) => None,
            (false, Some(mut b)) => {
                *b = s;
                Some(b)
            }
            (false, None) => Some(Box::new(s)),
        };
        // Written again as it was (a view presenting each turn) is no change to a client.
        if changed {
            self.access_touch(i);
        }
    }

    /// Draws (or stops drawing) the focus visual around `id` (`Ui`'s keyboard focus).
    pub(crate) fn set_ring(&mut self, id: Id, on: bool) {
        if self.alive(id) && self.els[id.idx as usize].ring != on {
            self.els[id.idx as usize].ring = on;
            self.why(id.idx, "focus visual");
            self.flag(id.idx, PAINT);
        }
    }

    pub fn set_tag(&mut self, id: Id, tag: u64) {
        if self.alive(id) {
            self.els[id.idx as usize].tag = tag;
        }
    }

    /// Asks for (or stops asking for) one kind of pointer input (`Listen`).
    pub fn set_listen(&mut self, id: Id, listen: Listen, on: bool) {
        if self.alive(id) {
            let e = &mut self.els[id.idx as usize];
            match listen {
                Listen::Hover => e.hover_events = on,
                Listen::Press => e.press_events = on,
                Listen::Wheel => e.wheel_events = on,
                Listen::Drag => e.drag_events = on,
            }
            self.access_touch(id.idx);
        }
    }

    // ---- images ---------------------------------------------------------------------------

    /// Notes the images element `i` draws: one not yet on the backend is uploaded next frame.
    fn use_images(&mut self, i: u32) {
        let e = &self.els[i as usize];
        let was = e.draws_images;
        let mut has = false;
        for h in e.content.images() {
            has = true;
            let id = h.id();
            match self.images.get_mut(&id) {
                Some(u) if u.weak.alive() => {
                    if !u.users.contains(&i) {
                        u.users.push(i);
                    }
                }
                _ => {
                    self.images.insert(id, ImageUse { weak: h.downgrade(), users: vec![i], unshown_since: None });
                    self.image_uploads.push(id);
                }
            }
        }
        // Drawing an image, or no longer (or hidden, or shown again): who shows which may have
        // changed.
        self.images_moved |= has || was;
        self.els[i as usize].draws_images = has;
    }

    /// Which images a shown element draws: one none has drawn for `IMAGE_KEEP` gives its
    /// texture back (an element that records it again uploads it again). Runs when an element
    /// drawing images was recorded or removed, and on the first turn after the next such
    /// deadline; not every frame.
    /// Answers how many it freed.
    fn look_at_images(&mut self, now: f64, be: &mut dyn Backend) -> usize {
        let mut next: Option<f64> = None;
        let mut freed = 0;
        let els = &self.els;
        self.images.retain(|&id, u| {
            u.users.retain(|&i| {
                let e = &els[i as usize];
                e.alive && e.content.images().any(|h| h.id() == id)
            });
            if u.users.iter().any(|&i| !els[i as usize].hidden) {
                u.unshown_since = None;
                return true;
            }
            let since = *u.unshown_since.get_or_insert(now);
            if now - since >= IMAGE_KEEP {
                be.image_free(id);
                freed += 1;
                return false;
            }
            next = Some(next.map_or(since + IMAGE_KEEP, |n| n.min(since + IMAGE_KEEP)));
            true
        });
        self.image_free.set(next);
        freed
    }

    /// Elements that draw an image the backend made off the UI thread since the last turn paint
    /// again (`Backend::image_make`): it shows from the frame after it was asked for.
    pub(crate) fn take_made_images(&mut self, be: &mut dyn Backend) {
        let mut made = std::mem::take(&mut self.images_made);
        be.images_made(&mut made);
        // The elements that noted each image (`ImageUse::users`), not a walk of the tree; one whose
        // content changed since draws it no more.
        let mut users = std::mem::take(&mut self.walk);
        for id in made.drain(..) {
            users.clear();
            users.extend(self.images.get(&id).map_or(&[][..], |u| &u.users[..]));
            for &i in &users {
                let e = &self.els[i as usize];
                if e.alive && e.content.images().any(|h| h.id() == id) {
                    self.flag(i, PAINT);
                }
            }
        }
        self.walk = users;
        self.images_made = made;
    }

    // ---- window ---------------------------------------------------------------------------

    pub fn resize(&mut self, w: i32, h: i32, scale: f32) {
        if (w, h, scale) == (self.w, self.h, self.scale) {
            return;
        }
        let rescale = scale != self.scale;
        self.w = w;
        self.h = h;
        self.scale = scale;
        self.resized = true;
        self.layout_dirty = true;
        self.damage.add(Rect::new(0, 0, w, h));
        if rescale {
            self.touch_everything();
        }
    }

    /// Every element is measured, laid out and painted again (a new scale, a new font).
    fn touch_everything(&mut self) {
        self.touch_all = true;
        self.layout_dirty = true;
        self.damage_window();
        for i in 0..self.els.len() as u32 {
            if self.els[i as usize].alive {
                self.flag(i, PAINT);
                let n = self.els[i as usize].node;
                taffy_ok(self.taffy.mark_dirty(n));
            }
        }
    }

    /// Puts an installed family first in the text's fallback order (`Text::set_font`); every
    /// text is measured and drawn again. False, and nothing changes, when it does not load.
    pub fn set_font(&mut self, family: Option<&str>) -> bool {
        if self.text.font() == family {
            return true;
        }
        if !self.text.set_font(family) {
            return false;
        }
        self.touch_everything();
        true
    }

    /// Sets the window-edge line (see `frame_stroke`); the edges are redrawn when it changes.
    pub fn set_frame_stroke(&mut self, c: Option<Color>) {
        if self.frame_stroke != c {
            self.frame_stroke = c;
            let (w, h) = (self.w, self.h);
            for r in
                [Rect::new(0, 0, w, 1), Rect::new(0, h - 1, w, 1), Rect::new(0, 0, 1, h), Rect::new(w - 1, 0, 1, h)]
            {
                self.damage.add(r);
            }
        }
    }

    pub fn expose(&mut self) {
        self.exposed = true;
    }

    /// The window rect of an element, in physical pixels.
    pub fn rect(&self, id: Id) -> Rect {
        if self.alive(id) {
            let e = &self.els[id.idx as usize];
            let (dx, dy) = self.win_off(e.grp);
            e.abs.translate(dx, dy)
        } else {
            Rect::EMPTY
        }
    }

    /// Own painted bounds in window pixels after a frame (not its layout box or descendants).
    /// Useful for paint-only controls such as a scrollbar thumb.
    pub fn painted_rect(&self, id: Id) -> Rect {
        if !self.alive(id) {
            return Rect::EMPTY;
        }
        let e = &self.els[id.idx as usize];
        if e.painted.is_empty() {
            Rect::EMPTY
        } else {
            let (dx, dy) = self.win_off(e.grp);
            e.painted.translate(e.abs.x - e.trect.x + dx, e.abs.y - e.trect.y + dy)
        }
    }

    /// Commit pending layout before reading geometry-dependent application state. No paint,
    /// upload or present occurs here; work is still damage-tracked for the next frame.
    /// CPU time for an application-initiated call belongs to that application's update phase.
    pub fn resolve_layout(&mut self) -> bool {
        if !self.layout_dirty {
            return false;
        }
        crate::sched::laid_out();
        let _layout = crate::sched::enter(crate::sched::Phase::Layout);
        if self.structure {
            self.rebuild_lists();
        }
        self.run_layout();
        self.layout_dirty = false;
        // Everything whose layout inputs changed, and its ancestors, is walked; elsewhere a
        // subtree whose context and box are as they were is skipped.
        // On the path: `on_path[i] == path_gen` (a new stamp per layout, nothing cleared).
        self.path_gen = self.path_gen.wrapping_add(1);
        if self.path_gen == 0 {
            self.on_path.iter_mut().for_each(|g| *g = 0);
            self.path_gen = 1;
        }
        self.walk_all = std::mem::take(&mut self.touch_all);
        if !self.walk_all {
            if self.on_path.len() < self.els.len() {
                self.on_path.resize(self.els.len(), 0);
            }
            let (gen, n) = (self.path_gen, self.touched.len());
            for k in 0..n + self.pos_dirty.len() {
                let mut i = if k < n { self.touched[k] } else { self.pos_dirty[k - n] };
                while (i as usize) < self.on_path.len() && self.on_path[i as usize] != gen {
                    self.on_path[i as usize] = gen;
                    match self.els[i as usize].parent {
                        Some(p) => i = p,
                        None => break,
                    }
                }
            }
        }
        self.touched.clear();
        self.pos_dirty.clear();
        self.position(self.root.idx);
        self.path_gen = self.path_gen.wrapping_add(1);
        self.walk_all = false;
        true
    }

    /// Element size in dips (after layout).
    pub fn size(&self, id: Id) -> (f32, f32) {
        self.el(id).size
    }

    /// Swaps the icon of an icon element, keeping its size and cut-out (the sheet's).
    #[cfg_attr(any(debug_assertions, test), track_caller)]
    pub fn set_icon(&mut self, id: Id, icon: nova_icons::Icon) {
        if !self.alive(id) {
            return;
        }
        let c = match &self.els[id.idx as usize].content {
            Content::Icon(c) if c.icon == icon => return,
            Content::Icon(c) => IconContent { icon, named: true, ..*c },
            _ => IconContent { icon, size: 16.0, cut: None, named: true },
        };
        self.set_content(id, Content::Icon(c));
    }

    // ---- animation ------------------------------------------------------------------------

    /// Returns the seconds the motion takes: a running one turned back toward where it came
    /// from takes the time it already went (`Tween::retarget`).
    pub fn animate(&mut self, id: Id, prop: Prop, to: f32, secs: f32, ease: Ease, now: f64) -> f32 {
        if !self.alive(id) {
            return 0.0;
        }
        let from = self.prop(id, prop);
        let running = self
            .slot(id)
            .and_then(|i| self.anims.iter().find(|a| a.el == i && a.prop == prop))
            .map(|a| a.tween)
            .filter(|t| !t.done(now));
        self.stop(id, prop);
        if secs <= 0.0 || (from - to).abs() < 1e-4 {
            self.set_prop(id, prop, to);
            return 0.0;
        }
        let tween = match running {
            Some(mut t) => {
                t.retarget(to, now, secs as f64, ease);
                t
            }
            None => crate::anim::Tween::new(from, to, now, secs as f64, ease),
        };
        self.anims.push(Anim { el: id.idx, prop, tween });
        tween.left(now) as f32
    }

    /// Ends an animation where it stands (the property keeps its current value).
    pub fn stop(&mut self, id: Id, prop: Prop) {
        if let Some(i) = self.slot(id) {
            self.anims.retain(|a| !(a.el == i && a.prop == prop));
        }
    }

    /// Entrance: the element comes from `from` (offset from rest) to rest. `from` is set first,
    /// so a running motion of these properties is replaced, not blended.
    pub fn enter(&mut self, id: Id, from: crate::anim::Pose, secs: f32, ease: Ease, now: f64) {
        self.keep(id);
        self.place_pose(id, from);
        self.animate_pose(id, from.rest(), secs, ease, now);
    }

    /// Entrance along `axes`' axes that continues from where the element stands (a reversed
    /// exit, a page turned back mid-way): no jump.
    pub fn reenter(&mut self, id: Id, axes: crate::anim::Pose, secs: f32, ease: Ease, now: f64) {
        self.animate_pose(id, axes.rest(), secs, ease, now);
    }

    /// Exit: from where the element stands to `to`. Returns when the motion ends. `then`: what
    /// happens at the end: `Stay` leaves it there (the caller's), `Hide` makes it
    /// `shown: false`, drops its children and resets its offset (a closed flyout); a hide is taken back by
    /// `enter`, `settle` or `keep`.
    pub fn exit(&mut self, id: Id, to: crate::anim::Pose, secs: f32, ease: Ease, now: f64, then: Then) -> f64 {
        let took = self.animate_pose(id, to, secs, ease, now);
        let end = now + f64::from(took);
        self.keep(id);
        if then == Then::Hide && self.alive(id) {
            self.leaving.push((id, end));
        }
        end
    }

    /// Shown and not on its way out (an entrance may still be running).
    pub fn is_up(&self, id: Id) -> bool {
        self.alive(id) && self.el(id).layout.display != Display::None && !self.is_leaving(id)
    }

    /// The element is on its way out (an `exit(.., Then::Hide)` running).
    pub fn is_leaving(&self, id: Id) -> bool {
        self.leaving.iter().any(|(i, _)| *i == id)
    }

    /// Cancels a pending hide: the element stays where its motion leaves it.
    pub fn keep(&mut self, id: Id) {
        self.leaving.retain(|(i, _)| *i != id);
    }

    /// At rest now: motions stopped, opaque, no offset.
    pub fn settle(&mut self, id: Id) {
        self.keep(id);
        self.place_pose(id, crate::anim::Pose { opacity: 1.0, dx: Some(0.0), dy: Some(0.0) });
    }

    fn place_pose(&mut self, id: Id, p: crate::anim::Pose) {
        self.stop(id, Prop::Opacity);
        self.set_opacity(id, p.opacity);
        for (prop, v) in [(Prop::X, p.dx), (Prop::Y, p.dy)] {
            if let Some(v) = v {
                self.stop(id, prop);
                self.set_prop(id, prop, v);
            }
        }
    }

    /// Returns the seconds the longest of its motions takes.
    fn animate_pose(&mut self, id: Id, p: crate::anim::Pose, secs: f32, ease: Ease, now: f64) -> f32 {
        let mut took = self.animate(id, Prop::Opacity, p.opacity, secs, ease, now);
        for (prop, v) in [(Prop::X, p.dx), (Prop::Y, p.dy)] {
            if let Some(v) = v {
                took = took.max(self.animate(id, prop, v, secs, ease, now));
            }
        }
        took
    }

    pub fn animating(&self) -> bool {
        !self.anims.is_empty() || !self.leaving.is_empty()
    }

    /// Running animations are the tree's only work. Their next frame differs from the last only
    /// once the clock has moved on.
    pub(crate) fn only_animating(&self) -> bool {
        !self.anims.is_empty() && !self.has_other_work()
    }

    pub fn is_animating(&self, id: Id, prop: Prop) -> bool {
        self.slot(id).is_some_and(|i| self.anims.iter().any(|a| a.el == i && a.prop == prop))
    }

    /// An animatable property's value now (a dead id: an empty element's).
    pub fn prop(&self, id: Id, prop: Prop) -> f32 {
        let e = self.el(id);
        match prop {
            Prop::Opacity => e.visual.opacity,
            Prop::X => e.translate.0,
            Prop::Y => e.translate.1,
            Prop::Scale => e.visual.scale,
            Prop::ScaleX => e.visual.scale_x,
            Prop::ScrollY => e.scroll.1,
            Prop::FgAlpha => e.visual.fg[3],
        }
    }

    pub fn set_prop(&mut self, id: Id, prop: Prop, v: f32) {
        match prop {
            Prop::Opacity => self.set_opacity(id, v),
            Prop::X => {
                let t = self.el(id).translate;
                self.set_translate(id, v, t.1)
            }
            Prop::Y => {
                let t = self.el(id).translate;
                self.set_translate(id, t.0, v)
            }
            Prop::Scale => self.set_visual(id, |vis| vis.scale = v),
            Prop::ScaleX => self.set_visual(id, |vis| vis.scale_x = v),
            Prop::ScrollY => {
                let s = self.el(id).scroll;
                self.set_scroll(id, s.0, v)
            }
            Prop::FgAlpha => self.set_visual(id, |vis| vis.fg[3] = v),
        }
    }

    /// Advances animations to `now`.
    pub fn tick(&mut self, now: f64) {
        // Compacted in place: the running ones stay at the front.
        let mut anims = std::mem::take(&mut self.anims);
        let mut kept = 0;
        for r in 0..anims.len() {
            let a = &anims[r];
            if !self.els[a.el as usize].alive {
                continue;
            }
            let (id, prop) = (self.id_of(a.el), a.prop);
            let (v, done) = a.sample(now);
            self.set_prop(id, prop, v);
            if !done {
                anims.swap(kept, r);
                kept += 1;
            }
        }
        anims.truncate(kept);
        // Animations started by setters during this tick are kept too.
        anims.append(&mut self.anims);
        self.anims = anims;
        self.serve_deadlines(now);
    }

    /// When the next element on its way out goes (a deadline like a timer's).
    pub(crate) fn next_leave(&self) -> Option<f64> {
        self.leaving.iter().map(|&(_, t)| t).reduce(f64::min)
    }

    /// Hides the elements whose exit ended by `now` (their subtree goes).
    pub(crate) fn serve_leaving(&mut self, now: f64) {
        if self.leaving.iter().any(|&(_, t)| now >= t) {
            let (done, wait): (Vec<_>, Vec<_>) =
                std::mem::take(&mut self.leaving).into_iter().partition(|&(_, t)| now >= t);
            self.leaving = wait;
            for (id, _) in done {
                if self.alive(id) {
                    self.set_layout(id, |l| l.display = Display::None);
                    self.clear_children(id);
                    self.set_translate(id, 0.0, 0.0);
                }
            }
        }
    }

    // ---- frame ----------------------------------------------------------------------------

    /// When a settled resize wants a frame to trim the backend (see `Backend::trim`), on the
    /// host clock.
    pub(crate) fn trim_at(&self) -> Option<f64> {
        self.trim.pending()
    }

    /// When an image no shown element draws may give its texture back, on the host clock. Not
    /// a wake: the first turn after it (any cause) frees it, so an idle window stays asleep.
    #[cfg(test)]
    pub(crate) fn image_free_at(&self) -> Option<f64> {
        self.image_free.pending()
    }

    /// The window's live resize (the modal size/move loop) ended: the backend trims again,
    /// `TRIM_AFTER` from `now`. A trim made while the border was held still was a live frame's,
    /// which keeps what the drag grew. Win32's alone: a Linux window has no modal loop and no
    /// live frames, and the trim after a resize's last frame (`frame`) is the settled one.
    #[cfg(windows)]
    pub(crate) fn resize_ended(&mut self, now: f64) {
        self.trim_after(now);
    }

    /// The backend's caches are trimmed `TRIM_AFTER` from `now`, unless the window resizes again.
    fn trim_after(&mut self, now: f64) {
        self.trim.set(Some(now + TRIM_AFTER));
    }

    /// The whole window is drawn again.
    fn damage_window(&mut self) {
        self.damage.add(Rect::new(0, 0, self.w, self.h));
    }

    /// Layout inputs of `i` changed: it is laid out again.
    fn touch(&mut self, i: u32) {
        self.touched.push(i);
        self.layout_dirty = true;
    }

    /// The children of `p` changed: the tree's structure moved, and `p` is laid out again.
    fn children_changed(&mut self, p: u32) {
        self.touch(p);
        self.access_touch(p);
        self.structure = true;
    }

    /// The layer of element `i`, if it has one, goes; the backend frees it with the frame.
    fn free_layer(&mut self, i: u32) {
        if self.layers.remove(&i).is_some() {
            self.freed_layers.push(i);
        }
    }

    /// Text deferred while off its clip (`text_pending`) now shows: its box, grown by what its
    /// ink overhangs, meets what is visible of `i`.
    fn pending_text_shows(&self, i: u32) -> bool {
        let e = &self.els[i as usize];
        e.trect.inflate(e.text_margin).intersects(&self.vis_clip(i))
    }

    /// The tree's own deadlines that came by `now`: exits that ended, a settled resize's trim.
    pub(crate) fn serve_deadlines(&mut self, now: f64) {
        self.serve_leaving(now);
        self.trim.serve(now);
        self.image_free.serve(now);
    }

    pub fn has_work(&self) -> bool {
        !self.anims.is_empty() || self.has_other_work()
    }

    /// `has_work` but for running animations.
    fn has_other_work(&self) -> bool {
        !self.changed.is_empty()
            || !self.damage.is_empty()
            || self.resized
            || self.trim.due
            || self.image_free.due
            || self.exposed
            || self.layout_dirty
            || !self.pos_dirty.is_empty()
            || !self.image_uploads.is_empty()
            || (!self.images.is_empty() && self.seen_drop_version != crate::image::DROP_VERSION.load(Ordering::Acquire))
            || self.layer_work
    }

    fn damage_in(&mut self, tgt: u32, r: Rect) {
        if r.is_empty() {
            return;
        }
        if tgt == 0 {
            self.damage.add(r);
        } else if let Some(l) = self.layers.get_mut(&tgt) {
            l.damage.add(r);
            self.layer_work = true;
        }
    }

    /// Damages where element `i`'s own item shows now: what it painted, in its offset group, on
    /// its target.
    fn damage_own(&mut self, i: u32) {
        let e = &self.els[i as usize];
        self.damage_grp(e.lgrp, e.tgt, e.painted);
    }

    /// Damages where layer `i`'s composite (its quad and shadow) shows now: in the group and on
    /// the target it is composited into.
    fn damage_comp(&mut self, i: u32) {
        let e = &self.els[i as usize];
        self.damage_grp(e.cgrp, e.ptgt, e.comp_shown());
    }

    /// Damages where everything under element `i` shows now: each descendant's own item, a layer
    /// by its composite (what is inside it stays in it). Before a change of how they are drawn
    /// (a group made, a layer made); their new places are damaged when they are painted there.
    fn damage_under(&mut self, i: u32) {
        let mut stack = std::mem::take(&mut self.walk);
        stack.clear();
        stack.extend(self.els[i as usize].children.iter().copied());
        while let Some(k) = stack.pop() {
            if self.els[k as usize].promoted {
                self.damage_comp(k);
            } else {
                stack.extend(self.els[k as usize].children.iter().copied());
                self.damage_own(k);
            }
        }
        self.walk = stack;
    }

    /// Damages `r`, given in the space of offset group `g`'s content (`NONE`: of target `tgt`).
    fn damage_grp(&mut self, mut g: u32, tgt: u32, mut r: Rect) {
        while g != NONE && !r.is_empty() {
            let e = &self.els[g as usize];
            r = r.translate(e.goff.0, e.goff.1).intersect(&e.gclip);
            g = e.lgrp;
        }
        self.damage_in(tgt, r);
    }

    /// The sum of the group offsets from `g` out (`g`'s content space to the window's).
    fn win_off(&self, mut g: u32) -> (i32, i32) {
        let (mut x, mut y) = (0, 0);
        while g != NONE {
            let e = &self.els[g as usize];
            (x, y) = (x + e.goff.0, y + e.goff.1);
            g = e.grp;
        }
        (x, y)
    }

    /// What of group `g`'s content space its target shows (`NONE`: all of it).
    fn group_vis(&self, g: u32) -> Rect {
        if g == NONE {
            return Rect::HUGE;
        }
        let e = &self.els[g as usize];
        let outer = self.group_vis(e.lgrp).intersect(&e.gclip);
        outer.translate(-e.goff.0, -e.goff.1)
    }

    /// The clip an element's own item is finally drawn under, in its list's space.
    fn vis_clip(&self, i: u32) -> Rect {
        let e = &self.els[i as usize];
        if e.lgrp == NONE {
            e.tclip
        } else {
            e.tclip.intersect(&self.group_vis(e.lgrp))
        }
    }

    fn run_layout(&mut self) {
        let (w, h, s) = (self.w as f32 / self.scale, self.h as f32 / self.scale, self.scale);
        let els = &self.els;
        let text = &mut self.text;
        let root = self.els[self.root.idx as usize].node;
        taffy_ok(self.taffy.compute_layout_with_measure(
            root,
            taffy::Size { width: AvailableSpace::Definite(w), height: AvailableSpace::Definite(h) },
            |inputs, _node, ctx, style| {
                let idx = ctx.copied();
                taffy::compute_leaf_layout(
                    inputs,
                    style,
                    |_, _| 0.0,
                    |known, avail| {
                        let Some(e) = idx.map(|i| &els[i as usize]) else { return taffy::Size::ZERO };
                        match &e.content {
                            Content::Text(t) => {
                                let st = t.style(s);
                                let width = known.width.or(match avail.width {
                                    AvailableSpace::Definite(v) => Some(v),
                                    _ => None,
                                });
                                if t.wrap {
                                    let min = matches!(avail.width, AvailableSpace::MinContent);
                                    let wpx = if min { 1 } else { width.map_or(0, |v| text_width_px(v, s)) };
                                    let p = text.measure(&t.text, &st, wpx, t.lines);

                                    taffy::Size {
                                        width: known.width.unwrap_or(p.ink_w as f32 / s),
                                        height: known.height.unwrap_or(p.h as f32 / s),
                                    }
                                } else {
                                    let p = text.measure(&t.text, &st, 0, 1);
                                    let natural = p.ink_w as f32 / s;
                                    let wv = match avail.width {
                                        AvailableSpace::MinContent => 0.0,
                                        _ => natural,
                                    };
                                    taffy::Size {
                                        width: known.width.unwrap_or(wv),
                                        height: known.height.unwrap_or(p.h as f32 / s),
                                    }
                                }
                            }
                            Content::Icon(ic) => taffy::Size {
                                width: known.width.unwrap_or(ic.size),
                                height: known.height.unwrap_or(ic.size),
                            },
                            Content::Path(pc) => taffy::Size {
                                width: known.width.unwrap_or(pc.size),
                                height: known.height.unwrap_or(pc.size),
                            },
                            _ => taffy::Size { width: known.width.unwrap_or(0.0), height: known.height.unwrap_or(0.0) },
                        }
                    },
                )
            },
        ));
        self.stats.layout_runs += 1;
        self.layout_version += 1;
        self.geometry_version += 1;
    }

    /// A new empty draw list (one kept from an earlier rebuild when there is one).
    fn new_list(&mut self) -> u32 {
        if self.n_lists == self.lists.len() {
            self.lists.push(List::default());
        }
        self.lists[self.n_lists].items.clear();
        self.n_lists += 1;
        (self.n_lists - 1) as u32
    }

    /// The draw list element `i` owns (the canvas's for the root).
    fn list_of(&self, i: u32) -> Option<usize> {
        let l = self.els[i as usize].own;
        (l != NONE).then_some(l as usize)
    }

    fn rebuild_lists(&mut self) {
        self.n_lists = 0;
        self.order.clear();
        self.materials.clear();
        let canvas = self.new_list();
        let root = self.root.idx;
        self.els[root as usize].own = canvas;
        // (element, list, depth, the group whose list `list` is, the innermost group above)
        let mut stack = std::mem::take(&mut self.build_stack);
        stack.clear();
        stack.push((root, canvas, 0u32, NONE, NONE));
        let mut seen_layers = std::mem::take(&mut self.seen_layers);
        seen_layers.clear();
        while let Some((i, list, depth, lg, g)) = stack.pop() {
            let e = &self.els[i as usize];
            if !e.alive {
                continue;
            }
            let (promoted, group) = (e.promoted, e.group && !e.promoted);
            let at = self.lists[list as usize].items.len() as u32;
            let own = if promoted || group {
                self.new_list()
            } else if i == root {
                canvas
            } else {
                NONE
            };
            let e = &mut self.els[i as usize];
            e.grp = g;
            e.own = own;
            if promoted {
                e.comp_slot = at;
                e.comp_list = list;
                e.cgrp = lg;
                e.slot = 0;
                e.slot_list = own;
                e.lgrp = NONE;
            } else {
                e.slot = at;
                e.slot_list = list;
                e.lgrp = lg;
                e.comp_list = u32::MAX;
                e.cgrp = NONE;
            }
            if group {
                e.gslot = at + 1;
                e.gslot_list = list;
            } else {
                e.gslot_list = u32::MAX;
            }
            let e = &self.els[i as usize];
            self.order.push(i);
            if e.visual.material.is_some() {
                self.materials.push(i);
            }
            // What the children's items go into.
            let (kids_list, own_lg, own_g) = if promoted {
                self.lists[list as usize].items.push(Item::Layer(i));
                self.lists[own as usize].items.push(Item::El(i));
                seen_layers.push((i, depth + 1));
                (own, NONE, g)
            } else if group {
                let l = &mut self.lists[list as usize].items;
                l.push(Item::El(i));
                l.push(Item::Group(i));
                (own, i, i)
            } else {
                self.lists[list as usize].items.push(Item::El(i));
                (list, lg, g)
            };
            let d = if promoted { depth + 1 } else { depth };
            for &c in self.els[i as usize].children.iter().rev() {
                stack.push((c, kids_list, d, own_lg, own_g));
            }
        }
        self.build_stack = stack;
        // Lists past the current ones keep their capacity, not their items.
        for l in &mut self.lists[self.n_lists..] {
            l.items.clear();
        }
        // Rastered innermost first: a layer's composite is drawn into the layer around it.
        seen_layers.sort_unstable_by_key(|&(l, d)| std::cmp::Reverse((d, l)));
        self.layer_order.clear();
        self.layer_order.extend(seen_layers.iter().map(|&(i, _)| i));
        self.layer_work = true;
        for &(i, depth) in &seen_layers {
            let l = self.layers.entry(i).or_insert(Layer {
                w: 0,
                h: 0,
                depth,
                damage: Damage::default(),
                allocated: false,
            });
            l.depth = depth;
        }
        self.seen_layers = seen_layers;
        for l in &mut self.lists[..self.n_lists] {
            l.chunks.clear();
            l.chunks.resize(l.items.len().div_ceil(CHUNK), (Rect::EMPTY, true));
        }
        self.structure = false;
        self.geometry_version += 1;
    }

    /// Positions the subtree at `i` given its parent's context.
    fn position(&mut self, start: u32) {
        // Context of `start` from its parent.
        let s = self.scale;
        let ctx = match self.els[start as usize].parent {
            None => Ctx { tclip: Rect::new(0, 0, self.w, self.h), wclip: Rect::HUGE, ..NO_CTX },
            Some(p) => self.els[p as usize].kid,
        };
        let mut stack = std::mem::take(&mut self.pos_stack);
        stack.clear();
        stack.push((start, ctx));
        while let Some((i, c)) = stack.pop() {
            let node = self.els[i as usize].node;
            let lay = self
                .taffy
                .layout(node)
                .map(|l| (l.location.x, l.location.y, l.size.width, l.size.height))
                .unwrap_or_default();
            let key = PosKey { c, lay };
            // Same context, same box, nothing changed under it: the subtree stands as it was.
            if i != start
                && self.els[i as usize].pkey == Some(key)
                && !self.walk_all
                && self.on_path.get(i as usize).is_none_or(|&g| g != self.path_gen)
            {
                continue;
            }
            {
                self.stats.positioned += 1;
            }
            let e = &mut self.els[i as usize];
            e.pkey = Some(key);
            // A group moves in whole pixels, after rounding, so its content rounds as at rest.
            let group = e.group && !e.promoted;
            let (gx, gy) =
                if group { ((e.translate.0 * s).round() as i32, (e.translate.1 * s).round() as i32) } else { (0, 0) };
            let (tx, ty) = if group { (0.0, 0.0) } else { e.translate };
            let x = c.x + lay.0 + tx;
            let y = c.y + lay.1 + ty;
            // Text is laid out in its box's exact (unrounded) size: the wrap width and the
            // vertical centring come from it, not from the pixel rect. A box that grows or shrinks
            // by less than a pixel keeps its rect but can still change where the text breaks or
            // ellipsizes, so a size change re-records text even when the rect stands.
            let text_resized = e.size != (lay.2, lay.3) && matches!(e.content, Content::Text(_));
            e.size = (lay.2, lay.3);
            let x0 = px(x, s);
            let y0 = px(y, s);
            // A layer's size comes from its box alone: from its edges, a translation by a fraction
            // of a pixel would make it a pixel wider or narrower, and a resized layer is rastered
            // again whole (frame after frame while it moves).
            let abs = if e.promoted {
                Rect::new(x0, y0, (lay.2 * s).round() as i32, (lay.3 * s).round() as i32)
            } else {
                Rect::new(x0 + gx, y0 + gy, px(x + lay.2, s) - x0, px(y + lay.3, s) - y0)
            };
            // Inside a moving layer an element's target rect stands while its box moves: the
            // accessibility tree hears every box that moved, flagged or not.
            let boxed = e.abs != abs;
            e.abs = abs;
            e.wclip = c.wclip;
            let hidden = c.hidden || !e.shows_itself();
            let old = (e.trect, e.tclip, e.opacity, e.hidden, e.tgt);
            let mut comp_change = None;
            let (child_origin, child_tgt, child_opacity);
            if e.promoted {
                let prect = abs.translate(-c.origin.0, -c.origin.1);
                let pop = c.opacity * e.visual.opacity;
                let changed_comp = prect != e.prect
                    || pop != e.popacity
                    || c.tclip != e.pclip
                    || hidden != e.hidden
                    || e.ptgt != c.tgt;
                if changed_comp {
                    comp_change = Some((e.ptgt, e.comp_shown()));
                }
                e.ptgt = c.tgt;
                e.prect = prect;
                e.pclip = c.tclip;
                e.popacity = pop;
                let pad = if e.visual.clip { 0 } else { (LAYER_PAD * s).ceil() as i32 };
                if pad != e.lpad {
                    comp_change = Some((e.ptgt, e.comp_shown()));
                    e.lpad = pad;
                }
                e.tgt = i;
                e.trect = Rect::new(pad, pad, abs.w, abs.h);
                // No primitive can draw beyond this render target, even without an explicit
                // element clip. Keep that limit in positioning so deferred text is revisited
                // when the target grows, not only when an explicit ancestor clip changes.
                e.tclip = Rect::new(0, 0, abs.w.max(1) + 2 * pad, abs.h.max(1) + 2 * pad);
                e.opacity = 1.0;
                child_origin = (abs.x - pad, abs.y - pad);
                child_tgt = i;
                child_opacity = 1.0;
            } else {
                e.tgt = c.tgt;
                e.trect = abs.translate(-c.origin.0, -c.origin.1);
                e.tclip = c.tclip;
                e.opacity = c.opacity * e.visual.opacity;
                child_origin = c.origin;
                child_tgt = c.tgt;
                child_opacity = e.opacity;
            }
            e.hidden = hidden;
            let new = (e.trect, e.tclip, e.opacity, e.hidden, e.tgt);
            let mut flag = if text_resized
                || old.0.w != new.0.w
                || old.0.h != new.0.h
                || old.2 != new.2
                || old.3 != new.3
                || old.4 != new.4
            {
                PAINT
            } else if old.0 != new.0 || old.1 != new.1 {
                // Inherited clipping is applied by collect(), not baked into local primitives.
                // Refresh clipped bounds/damage without re-recording fixed-size descendants.
                MOVE
            } else {
                0
            };
            let pending = e.text_pending && flag & MOVE != 0;
            let clip_self = e.visual.clip;
            let mut child_tclip = if clip_self { e.tclip.intersect(&e.trect) } else { e.tclip };
            let mut child_wclip = if clip_self { c.wclip.intersect(&abs) } else { c.wclip };
            // A layer's content is laid on its pixel origin: moved by a fraction of a pixel, the
            // children would round to other pixels inside it and the layer would be rastered
            // again every frame of the motion. The composite is pixel-aligned anyway.
            let (fx, fy) = if e.promoted { (x - abs.x as f32 / s, y - abs.y as f32 / s) } else { (0.0, 0.0) };
            let (mut cx, mut cy) = (x - fx - e.scroll.0, y - fy - e.scroll.1);
            // A group's content is laid out as if it stood still and unscrolled, unclipped by
            // what is around it: its offset and those clips are applied when it is drawn.
            let mut moved = None;
            if group {
                let goff = (gx - (e.scroll.0 * s).round() as i32, gy - (e.scroll.1 * s).round() as i32);
                if goff != e.goff || child_tclip != e.gclip {
                    moved = Some((e.goff, e.gclip));
                }
                (e.goff, e.gclip, e.gwclip) = (goff, child_tclip, child_wclip);
                (child_tclip, child_wclip) = (Rect::HUGE, Rect::HUGE);
                (cx, cy) = (x, y);
            }
            let kid = Ctx {
                x: cx,
                y: cy,
                origin: child_origin,
                tgt: child_tgt,
                tclip: child_tclip,
                wclip: child_wclip,
                opacity: child_opacity,
                hidden,
            };
            e.kid = kid;
            let promoted = e.promoted;
            let cg = e.cgrp;
            if pending && self.pending_text_shows(i) {
                flag = PAINT;
            }
            if flag != 0 {
                self.flag(i, flag);
            } else if boxed || moved.is_some() {
                self.access_touch(i);
            }
            if let Some((pt, r)) = comp_change {
                self.damage_grp(cg, pt, r);
                self.update_comp(i);
            }
            if let Some((off, clip)) = moved {
                self.group_moved(i, off, clip);
            }
            if promoted {
                let pad = self.els[i as usize].lpad;
                let (w, h) = (abs.w.max(1) + 2 * pad, abs.h.max(1) + 2 * pad);
                if let Some(l) = self.layers.get_mut(&i) {
                    if l.w != w || l.h != h {
                        l.w = w;
                        l.h = h;
                        l.allocated = false;
                        l.damage = Damage::default();
                        l.damage.add(Rect::new(0, 0, w, h));
                        self.layer_work = true;
                    }
                }
            }
            let kids = &self.els[i as usize].children;
            stack.extend(kids.iter().rev().map(|&k| (k, kid)));
        }
        self.pos_stack = stack;
    }

    fn record(&mut self, i: u32) {
        self.use_images(i);
        let s = self.scale;
        let mut prims = std::mem::take(&mut self.els[i as usize].prims);
        prims.clear();
        let mut text_pending = false;
        let mut text_margin = 0;
        let vis = self.vis_clip(i);
        let e = &self.els[i as usize];
        if !e.hidden && e.trect.w > 0 && e.trect.h > 0 {
            let (w, h) = (e.trect.w, e.trect.h);
            let v = &e.visual;
            let mut r = Rect::new(0, 0, w, h);
            if v.scale != 1.0 || v.scale_x != 1.0 {
                let (nw, nh) = ((w as f32 * v.scale * v.scale_x).round() as i32, (h as f32 * v.scale).round() as i32);
                r = Rect::new((w - nw) / 2, (h - nh) / 2, nw, nh);
            }
            let plate = if v.plate.is_some() { plate_rect(v.plate, Rect::new(0, 0, w, h), s) } else { r };
            let radius = v.radius * s * v.scale;
            let bg = if e.disabled {
                v.bg
            } else if e.lit {
                if e.pressed { v.bg_lit_press.or(v.bg_lit) } else { v.bg_lit }.or(v.bg)
            } else if e.focused && v.bg_focus.is_some() {
                v.bg_focus
            } else if e.pressed {
                v.bg_press.or(v.bg_hover).or(v.bg)
            } else if e.hovered {
                v.bg_hover.or(v.bg)
            } else {
                v.bg
            };
            // A layer's are drawn where it is composited (`collect_list`).
            if !e.promoted {
                under_plate(v, plate, radius, s, 1.0, |p| prims.push(p));
            }
            let border_color = if e.focused { v.border_focus.unwrap_or(v.border_color) } else { v.border_color };
            if bg.is_some() || v.border > 0.0 {
                prims.push(Prim::Quad {
                    rect: plate,
                    radius,
                    square: v.square,
                    fill: bg.unwrap_or(TRANSPARENT),
                    border: v.border * s,
                    border_color,
                });
            }
            // The bottom line: inside the rounded plate, clipped to it by drawing it as a thin
            // quad over the plate's flat part (radius-wide insets at the corners).
            let line = if e.focused {
                v.underline_focus.map(|c| (c, 2.0)).or(v.underline.map(|c| (c, 1.0)))
            } else {
                v.underline.map(|c| (c, 1.0))
            };
            if let Some((c, th)) = line {
                let th = (th * s).round().max(1.0) as i32;
                let inset = (radius * 0.5).round() as i32;
                prims.push(Prim::Quad {
                    rect: Rect::new(plate.x + inset, plate.b() - th, (plate.w - 2 * inset).max(0), th),
                    radius: 0.0,
                    square: 0,
                    fill: c,
                    border: 0.0,
                    border_color: TRANSPARENT,
                });
            }
            let fg = if e.disabled {
                v.fg_disabled.unwrap_or(v.fg)
            } else if e.off {
                v.fg_off.unwrap_or(v.fg)
            } else if e.hovered || e.pressed {
                v.fg_hover.unwrap_or(v.fg)
            } else {
                v.fg
            };
            match &e.content {
                Content::Text(t) => {
                    let st = t.style(s).scaled(v.scale);
                    let pad_px = (2.0 * s) as i32;
                    let outside = !v.underline_hover && !e.trect.inflate(pad_px).intersects(&vis);
                    if outside {
                        text_margin = pad_px + self.text.ink_margin(&t.text, t.weight, st.size);
                    }
                    if outside && !e.trect.inflate(text_margin).intersects(&vis) {
                        text_pending = true;
                    } else {
                        let pad = e.layout.padding;
                        let pl = (pad[3] * s).round() as i32;
                        // Layout constraints are local and translation-invariant. Rounded world
                        // edges may differ by a pixel as an ancestor moves; they are raster bounds,
                        // never a new text wrapping constraint.
                        let cw = text_width_px(e.size.0 - pad[3] - pad[1], s);
                        let para = self.text.layout(&t.text, &st, cw, t.max_lines(), t.align);
                        let content_h = ((e.size.1 - pad[0] - pad[2]).max(0.0) * s).round() as i32;
                        let y = (pad[0] * s).round() as i32 + (content_h - para.h) / 2;

                        let mut clip = Rect::new(0, 0, w, h).inflate((2.0 * s) as i32);
                        let sweep = v.sweep.map(|at| crate::style::Sweep { at, lit: v.sweep_fg });
                        let units = match &v.units {
                            Some(u) if u.fx.len() <= crate::style::UnitFx::MAX_UNITS => Some(u.clone()),
                            _ if v.text_rotate != 0.0 => Some(Default::default()),
                            _ => None,
                        };
                        match units {
                            Some(u) => {
                                // A unit moves at most a line, scales 2×, turns 45° and glows 16
                                // (`UnitFx::clamped`); the text turns about its centre and `text-origin`.
                                clip = clip.inflate((st.line.ceil() as i32 * 2).max(para.w / 2 + para.h));
                                let turn =
                                    (v.text_rotate, [v.text_origin[0] * s * v.scale, v.text_origin[1] * s * v.scale]);
                                // About a far origin the text sweeps an arc as wide as the origin is far.
                                let far = turn.1[0].abs().max(turn.1[1].abs()) * 2.0;
                                clip = clip.inflate(far.ceil() as i32);
                                self.text.prims_units(&para, pl, y, fg, sweep, &u, turn, s * v.scale, clip, &mut prims)
                            }
                            _ => match sweep {
                                Some(sw) => para.prims_swept(pl, y, fg, sw, clip, &mut prims),
                                None => para.prims(pl, y, fg, clip, &mut prims),
                            },
                        }
                        if v.underline_hover && e.hovered && para.ink_w > 0 {
                            let x0 = pl
                                + match t.align {
                                    crate::text::Align::Start => 0,
                                    crate::text::Align::Center => (cw - para.ink_w) / 2,
                                    crate::text::Align::End => cw - para.ink_w,
                                };
                            let uy = y + para.h - (2.0 * s).round() as i32;
                            prims.push(Prim::Quad {
                                rect: Rect::new(x0, uy, para.ink_w.min(w), s.round().max(1.0) as i32),
                                radius: 0.0,
                                square: 0,
                                fill: fg,
                                border: 0.0,
                                border_color: TRANSPARENT,
                            });
                        }
                    }
                }
                Content::Icon(ic) if ic.named => {
                    let (cx, cy) = (r.x as f32 + r.w as f32 * 0.5, r.y as f32 + r.h as f32 * 0.5);
                    self.text.mark_prims(ic.icon, ic.cut, ic.size * s * v.scale, cx, cy, fg, &mut prims);
                }
                Content::Path(pc) => {
                    let (cx, cy) = (r.x as f32 + r.w as f32 * 0.5, r.y as f32 + r.h as f32 * 0.5);
                    self.text.shape_prims(&pc.shape, pc.size * s * v.scale, cx, cy, fg, &mut prims);
                }
                // A size with no icon named: nothing to draw.
                Content::Icon(_) => {}
                Content::Image(a) => self.image_prim(r, radius, a, 1.0, &mut prims),
                Content::ImageCrossfade(x) => {
                    self.image_prim(r, radius, &x.from, 1.0 - x.mix, &mut prims);
                    self.image_prim(r, radius, &x.to, x.mix, &mut prims);
                }
                Content::Effect(a, b) => {
                    for f in std::iter::once(a).chain(b.iter()) {
                        prims.push(Prim::Effect {
                            rect: r,
                            image: f.image.id(),
                            effect: f.effect.0,
                            params: f.params,
                            opacity: f.opacity,
                        });
                    }
                }
                Content::None => {}
            }
            let op = e.opacity;
            if op < 1.0 {
                for p in prims.iter_mut() {
                    *p = p.faded(op);
                }
            }
            // Over the rest and never faded: an invisible hit area shows where the focus is.
            if e.ring {
                focus_visual(Rect::new(0, 0, w, h), v.radius * s, s, &mut prims);
            }
        }
        let local = prims.iter().fold(Rect::EMPTY, |a, p| a.union(&p.bounds()));
        let pages = prims.iter().fold(0, |m, p| match p {
            Prim::Glyph { uv, .. } | Prim::GlyphXf { uv, .. } => m | crate::text::page_mask(*uv),
            _ => m,
        });
        self.text.repage(self.els[i as usize].pages, pages);
        let e = &mut self.els[i as usize];
        e.pages = pages;
        e.prims = prims;
        e.local = local;
        e.text_pending = text_pending;
        e.text_margin = text_margin;
    }

    /// The damage audit (a contract, `audit`): every live element this frame's paint did not
    /// handle is recorded again and compared with what it shows, and its painted box with where it
    /// is now. A difference is a change that reached an element without its flag: a setter that
    /// missed `PAINT` or `MOVE` (`runtime.md`, "What each phase may do"). What it shows is left
    /// as it was.
    fn audit_damage(&mut self) {
        let mut fresh = vec![false; self.els.len()];
        for &i in &self.audit_fresh {
            fresh[i as usize] = true;
        }
        for i in 0..self.els.len() as u32 {
            if fresh[i as usize] || !self.els[i as usize].alive {
                continue;
            }
            let e = &self.els[i as usize];
            let kept = (e.prims.clone(), e.local, e.text_pending, e.text_margin);
            let kept_pages = e.pages;
            let painted = e.shown(kept.1);
            self.record(i);
            let e = &mut self.els[i as usize];
            // Text that scrolled out keeps what it drew (it is deferred, not stale).
            let same = e.prims == kept.0 || (e.text_pending && !kept.2);
            let (prims, local, pending, margin) = kept;
            let now = std::mem::replace(&mut e.prims, prims);
            let diff = (!same).then(|| (now, what(e)));
            e.local = local;
            e.text_pending = pending;
            e.text_margin = margin;
            let now_pages = std::mem::replace(&mut e.pages, kept_pages);
            self.text.repage(now_pages, kept_pages);
            if self.text.reset || self.text.has_stale() {
                // The atlas filled while recording again: tiles moved, nothing can be compared.
                // This frame paints everything with text again.
                self.paint();
                return;
            }
            let e = &self.els[i as usize];
            if let Some((now, kind)) = diff {
                panic!(
                    "damage audit: element {i} ({kind}) draws differently than it shows, without a paint flag \
                     (a setter missed PAINT)\n shows: {:?}\n draws: {now:?}",
                    e.prims
                );
            }
            assert!(
                painted == e.painted,
                "damage audit: element {i} ({}) is at {painted:?} but shows at {:?}, without a move flag (a setter missed MOVE)",
                what(e),
                e.painted
            );
        }
    }

    fn image_prim(&self, r: Rect, radius: f32, a: &ImageContent, k: f32, out: &mut Vec<Prim>) {
        let (iw, ih) = a.image.size();
        let uv = if a.fit_square && iw != ih {
            let side = iw.min(ih) as f32;
            let (u, v) = ((iw as f32 - side) * 0.5 / iw as f32, (ih as f32 - side) * 0.5 / ih as f32);
            [u, v, 1.0 - u, 1.0 - v]
        } else {
            [0.0, 0.0, 1.0, 1.0]
        };
        if a.opacity * k > 0.0 {
            let hdr = a.hdr && a.image.has_gain_map();
            out.push(Prim::Image { rect: r, image: a.image.id(), uv, radius, opacity: a.opacity * k, hdr });
        }
    }

    fn paint(&mut self) {
        if self.text.reset {
            self.text.reset = false;
            for i in 0..self.els.len() as u32 {
                if self.els[i as usize].alive
                    && matches!(self.els[i as usize].content, Content::Text(_) | Content::Icon(_) | Content::Path(_))
                {
                    self.flag(i, PAINT);
                }
            }
        }
        // Atlas pages evicted for others' glyphs: what showed glyphs from them is painted again.
        let stale = self.text.take_stale();
        if stale != 0 {
            for i in 0..self.els.len() as u32 {
                if self.els[i as usize].alive && self.els[i as usize].pages & stale != 0 {
                    self.flag(i, PAINT);
                }
            }
        }
        let changed = std::mem::take(&mut self.changed);
        for i in changed {
            let invalid = std::mem::take(&mut self.els[i as usize].invalid);
            if invalid == 0 || !self.els[i as usize].alive {
                continue;
            }
            if crate::audit::on() {
                self.audit_fresh.push(i);
            }
            let (tgt, old, lg) = {
                let e = &self.els[i as usize];
                (e.tgt, e.painted, e.lgrp)
            };
            if invalid & PAINT != 0 {
                {
                    self.stats.painted += 1;
                }
                self.record(i);
            } else {
                {
                    self.stats.moved += 1;
                }
            }
            let e = &mut self.els[i as usize];
            // Kept as drawn: clipped by the clip it was drawn under, so a later damage of the old
            // pixels covers all of them even after the clip has changed (a clipping parent that
            // moved inside a layer's margin).
            e.painted = e.shown(e.local);
            self.stale_chunk(i, false);
            // Where it was and where it is, in the group and on the target it is drawn in now.
            // When those changed (a layer or a group made above it), where it showed before was
            // damaged at the change (`damage_under`).
            self.damage_grp(lg, tgt, old);
            self.damage_own(i);
            if self.els[i as usize].promoted {
                self.update_comp(i);
            }
        }
    }

    /// A promoted element's composite (its layer quad and shadow in the parent target): recompute
    /// its bounds and damage them there. Its layer is not touched.
    fn update_comp(&mut self, i: u32) {
        let e = &mut self.els[i as usize];
        let mut cp = e.layer_dst();
        if let Some(sh) = e.visual.shadow {
            let shadow = if e.visual.plate.is_some() { plate_rect(e.visual.plate, e.prect, self.scale) } else { cp };
            cp = cp.union(
                &shadow
                    .translate(0, (sh.dy * self.scale).round() as i32)
                    .inflate((sh.sigma * self.scale * 3.0).ceil() as i32 + 1),
            );
        }
        e.comp_painted = if e.hidden { Rect::EMPTY } else { cp };
        self.stale_chunk(i, true);
        self.damage_comp(i);
    }

    fn stale_chunk(&mut self, i: u32, comp: bool) {
        let e = &self.els[i as usize];
        let (l, at, g) = if comp { (e.comp_list, e.comp_slot, e.cgrp) } else { (e.slot_list, e.slot, e.lgrp) };
        // A group's content item stands right after its own (it may start the next run).
        let content = (!comp).then_some((e.gslot_list, e.gslot));
        self.stale_at(l, at);
        if let Some((gl, ga)) = content {
            self.stale_at(gl, ga);
        }
        self.stale_group(g);
    }

    fn stale_at(&mut self, l: u32, at: u32) {
        if let Some(c) = self.lists.get_mut(l as usize).and_then(|l| l.chunks.get_mut(at as usize / CHUNK)) {
            c.1 = true;
        }
    }

    /// Group `g`'s content changed: the runs holding it, out to its target's list, are stale.
    fn stale_group(&mut self, mut g: u32) {
        while g != NONE {
            let (l, at, up) = {
                let e = &self.els[g as usize];
                (e.gslot_list, e.gslot, e.lgrp)
            };
            self.stale_at(l, at);
            g = up;
        }
    }

    /// The bounds of what list `key` paints, in its space; recomputes the runs that changed.
    fn list_bounds(&mut self, key: u32) -> Rect {
        let Some(li) = self.list_of(key) else { return Rect::EMPTY };
        let list = std::mem::take(&mut self.lists[li].items);
        let mut all = Rect::EMPTY;
        for k in 0..self.lists[li].chunks.len() {
            if self.lists[li].chunks[k].1 {
                let mut b = Rect::EMPTY;
                for it in &list[k * CHUNK..((k + 1) * CHUNK).min(list.len())] {
                    let r = match *it {
                        Item::El(i) => self.els[i as usize].painted,
                        Item::Layer(i) => self.els[i as usize].comp_painted,
                        Item::Group(g) => self.group_bounds(g),
                    };
                    b = b.union(&r);
                }
                self.lists[li].chunks[k] = (b, false);
            }
            all = all.union(&self.lists[li].chunks[k].0);
        }
        self.lists[li].items = list;
        all
    }

    /// What group `g`'s content paints, in the space of the list its item is in.
    fn group_bounds(&mut self, g: u32) -> Rect {
        if self.els[g as usize].hidden {
            return Rect::EMPTY;
        }
        let b = self.list_bounds(g);
        let e = &self.els[g as usize];
        b.translate(e.goff.0, e.goff.1).intersect(&e.gclip)
    }

    /// Group `g` moved (or its clip changed) from `old_off` / `old_clip`: its content's old and
    /// new places are damaged, and text it had left unrecorded off its clip comes into view.
    fn group_moved(&mut self, g: u32, old_off: (i32, i32), old_clip: Rect) {
        let b = self.list_bounds(g);
        let e = &self.els[g as usize];
        let (lg, tgt, off, clip) = (e.lgrp, e.tgt, e.goff, e.gclip);
        self.damage_grp(lg, tgt, b.translate(old_off.0, old_off.1).intersect(&old_clip));
        self.damage_grp(lg, tgt, b.translate(off.0, off.1).intersect(&clip));
        self.stale_group(g);
        self.reveal_text(g);
    }

    /// Records the deferred text in group `g`'s content that its clip now shows.
    fn reveal_text(&mut self, g: u32) {
        let Some(li) = self.list_of(g) else { return };
        let list = std::mem::take(&mut self.lists[li].items);
        for it in &list {
            match *it {
                Item::El(i) => {
                    let e = &self.els[i as usize];
                    if e.text_pending && !e.hidden && self.pending_text_shows(i) {
                        self.flag(i, PAINT);
                    }
                }
                Item::Group(h) => self.reveal_text(h),
                Item::Layer(_) => {}
            }
        }
        self.lists[li].items = list;
    }

    fn collect(&mut self, tgt: u32, region: Rect, out: &mut Vec<Draw>) {
        out.clear();
        // Run bounds first (recomputed where an item changed since).
        self.list_bounds(tgt);
        self.collect_list(tgt, region, out);
    }

    fn collect_list(&self, key: u32, region: Rect, out: &mut Vec<Draw>) {
        let Some(li) = self.list_of(key) else { return };
        let List { items: list, chunks } = &self.lists[li];
        let s = self.scale;
        let mut n = 0;
        while n < list.len() {
            // A run that paints nothing in the region is passed over whole.
            if n % CHUNK == 0 && chunks.get(n / CHUNK).is_some_and(|c| !c.0.intersects(&region)) {
                n += CHUNK;
                continue;
            }
            let it = &list[n];
            n += 1;
            match *it {
                Item::El(i) => {
                    let e = &self.els[i as usize];
                    if e.hidden || !e.painted.intersects(&region) {
                        continue;
                    }
                    let clip = e.tclip.intersect(&region);
                    if clip.is_empty() {
                        continue;
                    }
                    let (ox, oy) = (e.trect.x, e.trect.y);
                    let lr = clip.translate(-ox, -oy);
                    for p in &e.prims {
                        if p.bounds().intersects(&lr) {
                            out.push(Draw { prim: p.translated(ox, oy), clip });
                        }
                    }
                }
                Item::Layer(i) => {
                    let e = &self.els[i as usize];
                    if e.hidden || !e.comp_painted.intersects(&region) {
                        continue;
                    }
                    let clip = e.pclip.intersect(&region);
                    let plate = plate_rect(e.visual.plate, e.prect, s);
                    under_plate(&e.visual, plate, e.visual.radius * s, s, e.popacity, |prim| {
                        out.push(Draw { prim, clip })
                    });
                    if self.layers.get(&i).is_some_and(|l| l.allocated) {
                        out.push(Draw {
                            prim: Prim::Layer {
                                dst: e.layer_dst(),
                                layer: i,
                                src: (0, 0),
                                opacity: e.popacity,
                                fade: None,
                            },
                            clip,
                        });
                    }
                }
                Item::Group(g) => {
                    let e = &self.els[g as usize];
                    let r = region.intersect(&e.gclip);
                    if e.hidden || r.is_empty() {
                        continue;
                    }
                    let (dx, dy) = e.goff;
                    let k = out.len();
                    self.collect_list(g, r.translate(-dx, -dy), out);
                    for d in &mut out[k..] {
                        d.prim = d.prim.translated(dx, dy);
                        d.clip = d.clip.translate(dx, dy);
                    }
                }
            }
        }
    }

    /// One frame. Returns false when nothing was drawn.
    pub(crate) fn frame(&mut self, be: &mut dyn Backend, now: f64) -> bool {
        use crate::sched::{enter, phase, Phase};
        crate::audit::contract!(
            phase() == Phase::Idle,
            "a frame drawn inside a turn's dispatch, layout or after_layout"
        );
        let _frame = enter(Phase::Frame);
        let audit = crate::audit::on();
        let (paras0, glyphs0) = (self.text.stats_laid_out, self.text.stats_rasterized);
        self.tick(now);
        self.text.between_frames();
        let t_start = std::time::Instant::now();
        let us = |t: std::time::Instant| t.elapsed().as_micros() as u64;
        if self.structure {
            self.rebuild_lists();
        }
        if !self.resolve_layout() && !self.pos_dirty.is_empty() {
            let mut roots = std::mem::take(&mut self.pos_dirty);
            roots.sort_unstable();
            roots.dedup();
            for r in roots {
                if self.els[r as usize].alive {
                    self.position(r);
                }
            }
        }
        if self.structure {
            self.rebuild_lists();
            let r = self.root.idx;
            self.position(r);
        }

        self.stats.us_layout = us(t_start);
        let t_paint = std::time::Instant::now();
        if audit {
            self.audit_fresh.clear();
        }
        self.paint();
        // The atlas evicted pages part-way through this paint, or started over: what showed
        // glyphs from them points at cleared tiles. Paint those again, in this same frame. A
        // page used this frame is never evicted, so this settles.
        for _ in 0..4 {
            if !self.text.reset && !self.text.has_stale() {
                break;
            }
            self.paint();
        }
        self.stats.us_paint = us(t_paint);
        if audit {
            self.audit_damage();
        }
        let t_draw = std::time::Instant::now();

        if std::mem::take(&mut self.resized) {
            be.resize(self.w, self.h);
            self.trim_after(now);
        } else if self.trim.take_due() {
            self.trim.set(None);
            be.trim();
        }
        for id in self.freed_layers.drain(..) {
            be.layer_free(id);
        }
        // A texture goes the first frame after its image's last handle did.
        let drops = crate::image::DROP_VERSION.load(Ordering::Acquire);
        if drops != self.seen_drop_version {
            self.seen_drop_version = drops;
            self.images.retain(|&id, u| {
                u.weak.alive() || {
                    be.image_free(id);
                    false
                }
            });
        }
        let due = self.image_free.take_due();
        if std::mem::take(&mut self.images_moved) || due {
            let freed = self.look_at_images(now, be);
            if due && freed > 0 {
                // The deadline's frees are for a window that went still: the driver keeps
                // released textures in its pools until told to let them go.
                be.release_pools();
            }
        }
        for id in std::mem::take(&mut self.image_uploads) {
            if let Some(h) = self.images.get(&id).and_then(|u| u.weak.upgrade()) {
                be.image_make(&h, self.image_wake.as_ref());
                self.stats.images_up += 1;
            }
        }
        for r in self.text.take_uploads() {
            be.upload_atlas(r, &self.text.atlas, crate::text::ATLAS);
            {
                self.stats.atlas_px += r.area();
            }
        }
        // Layers, innermost first.
        let mut scratch = std::mem::take(&mut self.scratch);
        let mut rects = std::mem::take(&mut self.layer_rects);
        for k in 0..if self.layer_work { self.layer_order.len() } else { 0 } {
            let id = self.layer_order[k];
            // A hidden layer is neither allocated nor rastered: its texture is not composited,
            // and what it would draw is drawn whole when it shows (its damage is kept). A
            // resize no longer repaints the pages behind the one shown, the closed menus, the
            // toast.
            if self.els[id as usize].hidden {
                continue;
            }
            let Some(l) = self.layers.get_mut(&id) else { continue };
            if !l.allocated {
                {
                    self.stats.layer_allocs += 1;
                }
                be.layer_alloc(id, l.w, l.h);
                l.allocated = true;
                l.damage = Damage::default();
                l.damage.add(Rect::new(0, 0, l.w, l.h));
            }
            l.damage.take_into(&mut rects);
            let bounds = Rect::new(0, 0, l.w, l.h);
            for &r in &rects {
                let r = r.intersect(&bounds);
                if r.is_empty() {
                    continue;
                }
                self.collect(id, r, &mut scratch);
                {
                    self.stats.prims += scratch.len() as u64;
                }
                {
                    self.stats.layer_px += r.area();
                }
                {
                    self.stats.draws += 1;
                }
                be.draw(Target::Layer(id), r, (0, 0), TRANSPARENT, &scratch);
                let e = &self.els[id as usize];
                let lr = e.layer_dst();
                let (pt, pr, pc, cg) = (e.ptgt, r.translate(lr.x, lr.y).intersect(&lr), e.pclip, e.cgrp);
                self.damage_grp(cg, pt, pr.intersect(&pc));
            }
        }
        self.layer_rects = rects;
        // What a hidden layer holds waits: showing it is a change of its own.
        self.layer_work = false;
        let win = Rect::new(0, 0, self.w, self.h);
        let mut dmg = std::mem::take(&mut self.drawn);
        self.damage.take_into(&mut dmg);
        dmg.retain_mut(|r| {
            *r = r.intersect(&win);
            !r.is_empty()
        });
        let mut reach = std::mem::take(&mut self.reach);
        self.widen_for_materials(&mut dmg, win, &mut reach);
        self.reach = reach;
        let exposed = std::mem::take(&mut self.exposed);
        let drew = !dmg.is_empty() || exposed;
        let clear = self.clear;
        for r in &dmg {
            self.collect(0, *r, &mut scratch);
            {
                self.stats.prims += scratch.len() as u64;
            }
            {
                self.stats.canvas_px += r.area();
            }
            {
                self.stats.draws += 1;
            }
            if let Some(c) = self.frame_stroke {
                // Only where the region reaches the window's edge.
                if r.x == 0 || r.y == 0 || r.r() >= win.w || r.b() >= win.h {
                    scratch.push(Draw {
                        prim: Prim::Quad {
                            rect: win,
                            radius: 0.0,
                            square: 0,
                            fill: TRANSPARENT,
                            border: 1.0,
                            border_color: c,
                        },
                        clip: *r,
                    });
                }
            }
            be.draw(Target::Canvas, *r, (0, 0), clear, &scratch);
        }
        self.scratch = scratch;
        if exposed {
            dmg.clear();
            dmg.push(win);
        }

        {
            self.stats.paras = self.text.stats_laid_out - paras0;
        }
        {
            self.stats.glyphs = self.text.stats_rasterized - glyphs0;
        }

        self.stats.us_draw = us(t_draw);
        let t_present = std::time::Instant::now();
        if drew {
            let full = [win];
            be.present(if exposed { &full } else { &dmg });
        }
        be.end_frame();
        self.stats.us_present = us(t_present); // Resource-only changes also release backend references without a present.
        {
            self.last = std::mem::take(&mut self.stats);
        }
        self.drawn = dmg;
        drew
    }

    /// `r` (unclipped) and `c` (a clip) from group `g`'s content space into its target's.
    fn out_of_groups(&self, mut g: u32, mut r: Rect, mut c: Rect) -> (Rect, Rect) {
        while g != NONE {
            let e = &self.els[g as usize];
            r = r.translate(e.goff.0, e.goff.1);
            c = c.translate(e.goff.0, e.goff.1).intersect(&e.gclip);
            g = e.lgrp;
        }
        (r, c)
    }

    /// A material reads what is drawn around it, up to its blur's reach: every damage rect that
    /// touches that reach must hold all of it, so the material samples this frame's pixels drawn
    /// before it in the same draw, never stale ones or its own plate.
    fn widen_for_materials(&self, dmg: &mut Vec<Rect>, win: Rect, reach: &mut Vec<Rect>) {
        let s = self.scale;
        reach.clear();
        reach.extend(
            self.materials
                .iter()
                .map(|&i| &self.els[i as usize])
                .filter(|e| e.alive && !e.hidden && e.visual.material.is_some() && e.visual.visible)
                .filter_map(|e| {
                    let m = e.visual.material?;
                    // The unclipped rect: the material samples around all of it.
                    let (tgt, r, c, g) =
                        if e.promoted { (e.ptgt, e.prect, e.pclip, e.cgrp) } else { (e.tgt, e.trect, e.tclip, e.lgrp) };
                    let r = plate_rect(e.visual.plate, r, s);
                    // Out of offset groups, into the target's pixels.
                    let (r, c) = self.out_of_groups(g, r, c);
                    (tgt == 0 && !r.intersect(&c).is_empty())
                        .then(|| r.inflate(crate::render::primitive::kawase(m.sigma * s).1).intersect(&win))
                }),
        );
        if reach.is_empty() {
            return;
        }
        loop {
            let mut changed = false;
            for g in reach.iter() {
                let mut hits = dmg.iter().filter(|d| d.intersects(g));
                let Some(first) = hits.next() else { continue };
                if hits.next().is_none() && first.intersect(g) == *g {
                    continue;
                }
                let u = dmg.iter().filter(|d| d.intersects(g)).fold(*g, |a, b| a.union(b));
                dmg.retain(|d| !d.intersects(g));
                dmg.push(u);
                changed = true;
            }
            if !changed {
                break;
            }
        }
    }

    /// Forgets every cache (reference rendering / device loss).
    pub fn invalidate_all(&mut self) {
        self.resized = true;
        self.damage_window();
        for i in 0..self.els.len() as u32 {
            if self.els[i as usize].alive {
                self.flag(i, PAINT);
            }
        }
        for l in self.layers.values_mut() {
            l.allocated = false;
        }
        self.layer_work = true;
        self.image_uploads = self.images.keys().copied().collect();
        self.text.reupload_all();
    }

    // ---- hit testing ----------------------------------------------------------------------

    /// Topmost element under (x, y) px for which `pred` holds.
    pub fn hit(&self, x: i32, y: i32, pred: impl Fn(&El) -> bool) -> Option<Id> {
        self.hit_tests.set(self.hit_tests.get() + 1);
        for &i in self.order.iter().rev() {
            self.hit_visits.set(self.hit_visits.get() + 1);
            let e = &self.els[i as usize];
            if !e.landable() {
                continue;
            }
            if self.under(e, x, y) && pred(e) {
                return Some(self.id_of(i));
            }
        }
        None
    }

    /// All elements under (x, y) px for which `pred` holds, topmost first.
    pub fn hits(&self, x: i32, y: i32, pred: impl Fn(&El) -> bool) -> Vec<Id> {
        let mut out = Vec::new();
        self.hit_tests.set(self.hit_tests.get() + 1);
        for &i in self.order.iter().rev() {
            self.hit_visits.set(self.hit_visits.get() + 1);
            let e = &self.els[i as usize];
            if !e.landable() {
                continue;
            }
            if self.under(e, x, y) && pred(e) {
                out.push(self.id_of(i));
            }
        }
        out
    }

    /// Whether window point (x, y) px is on `e` and inside every clip above it.
    fn under(&self, e: &El, x: i32, y: i32) -> bool {
        if e.grp == NONE {
            return e.abs.contains(x, y) && e.wclip.contains(x, y);
        }
        let (dx, dy) = self.win_off(e.grp);
        let (mut x, mut y) = (x - dx, y - dy);
        if !(e.abs.contains(x, y) && e.wclip.contains(x, y)) {
            return false;
        }
        let mut g = e.grp;
        while g != NONE {
            let ge = &self.els[g as usize];
            (x, y) = (x + ge.goff.0, y + ge.goff.1);
            if !ge.gwclip.contains(x, y) {
                return false;
            }
            g = ge.grp;
        }
        true
    }

    /// The element's parent, its parent's, and so on to the root.
    pub fn ancestors(&self, id: Id) -> impl Iterator<Item = Id> + '_ {
        let mut p = self.el(id).parent;
        std::iter::from_fn(move || {
            let i = p?;
            p = self.els[i as usize].parent;
            Some(self.id_of(i))
        })
    }

    /// `id` is `of` or inside it; false when either is dead.
    pub fn is_descendant(&self, id: Id, of: Id) -> bool {
        let Some(of) = self.slot(of) else { return false };
        let mut p = self.slot(id);
        while let Some(i) = p {
            if i == of {
                return true;
            }
            p = self.els[i as usize].parent;
        }
        false
    }
}

/// What stands under an element's plate: its material's backdrop first (it samples what is
/// behind), then its shadow, cut out under the plate when there is a material (a shadow drawn
/// before would be blurred into the glass). `opacity`: a layer's, where it is composited.
fn under_plate(v: &Visual, plate: Rect, radius: f32, s: f32, opacity: f32, mut push: impl FnMut(Prim)) {
    if let Some(m) = v.material {
        let (levels, extent) = crate::render::primitive::kawase(m.sigma * s);
        push(Prim::Backdrop { rect: plate, radius, levels, extent, tint: m.tint, lum: m.lum, opacity });
    }
    if let Some(sh) = v.shadow {
        let mut color = sh.color;
        color[3] *= opacity;
        push(Prim::Shadow {
            rect: plate.translate(0, (sh.dy * s).round() as i32),
            radius,
            sigma: sh.sigma * s,
            color,
            cut: v.material.map(|_| plate),
        });
    }
}

impl El {
    /// A new, live element with nothing set but what it is given.
    fn blank(node: NodeId, layout: Layout, visual: Visual, content: Content) -> El {
        El {
            gen: 0,
            alive: true,
            parent: None,
            children: Vec::new(),
            node,
            layout,
            visual,
            content,
            tag: 0,
            unit_src: None,
            hovered: false,
            focused: false,
            pressed: false,
            lit: false,
            disabled: false,
            off: false,
            hover_events: false,
            press_events: false,
            wheel_events: false,
            drag_events: false,
            editable: false,
            ring: false,
            action: None,
            semantics: None,
            promoted: false,
            translate: (0.0, 0.0),
            scroll: (0.0, 0.0),
            group: false,
            kid: NO_CTX,
            size: (0.0, 0.0),
            abs: Rect::EMPTY,
            wclip: Rect::HUGE,
            tgt: 0,
            trect: Rect::EMPTY,
            tclip: Rect::HUGE,
            opacity: 1.0,
            hidden: false,
            ptgt: 0,
            prect: Rect::EMPTY,
            pclip: Rect::HUGE,
            popacity: 1.0,
            lpad: 0,
            comp_painted: Rect::EMPTY,
            grp: NONE,
            lgrp: NONE,
            cgrp: NONE,
            goff: (0, 0),
            gclip: Rect::EMPTY,
            gwclip: Rect::EMPTY,
            gslot: 0,
            gslot_list: NONE,
            pkey: None,
            own: NONE,
            slot: 0,
            comp_slot: 0,
            slot_list: u32::MAX,
            comp_list: u32::MAX,
            invalid: PAINT,
            text_pending: false,
            draws_images: false,
            text_margin: 0,
            pages: 0,
            prims: Vec::new(),
            local: Rect::EMPTY,
            painted: Rect::EMPTY,
        }
    }

    /// Where primitives recorded at `local` (its own coordinates) show in its target: placed at
    /// its text rect, cut to its clip.
    fn shown(&self, local: Rect) -> Rect {
        if local.is_empty() {
            Rect::EMPTY
        } else {
            local.translate(self.trect.x, self.trect.y).intersect(&self.tclip)
        }
    }

    /// Where its composite shows in its parent's target: its composite's painted box, cut to the
    /// parent's clip.
    fn comp_shown(&self) -> Rect {
        self.comp_painted.intersect(&self.pclip)
    }

    /// Where its layer's texture lands: its box grown by the layer's margin.
    fn layer_dst(&self) -> Rect {
        self.prect.inflate(self.lpad)
    }

    /// The pointer can land on it (a click's target, a hover's): alive and shown, also when it
    /// was hidden this turn (before the frame that marks it `hidden`).
    pub(crate) fn landable(&self) -> bool {
        self.alive && !self.hidden && self.shows_itself()
    }

    /// It is set to show: visible and displayed (an ancestor may still hide it).
    pub(crate) fn shows_itself(&self) -> bool {
        self.visual.visible && self.layout.display != Display::None
    }

    /// Tab stops at it: written (`focusable`), else it can be pressed, dragged or typed in.
    pub fn focusable(&self) -> bool {
        match self.semantics.as_ref().and_then(|s| s.focusable) {
            Some(f) => f,
            None => self.action.is_some() || self.drag_events || self.editable,
        }
    }

    /// The keyboard can reach it now: focusable, enabled and shown (as of the last frame).
    pub(crate) fn reachable(&self) -> bool {
        self.landable() && self.focusable() && !self.disabled && !self.trect.is_empty()
    }

    /// The pointer can land on it: a target, or an element that hears hover.
    pub(crate) fn takes_pointer(&self) -> bool {
        self.is_target() || self.hover_events()
    }

    pub fn is_target(&self) -> bool {
        self.visual.hit
            || self.action.is_some()
            || self.visual.bg_hover.is_some()
            || self.visual.bg_press.is_some()
            || self.drag_events
            || self.editable
    }
}
