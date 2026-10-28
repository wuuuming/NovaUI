//! Element styles: layout (Taffy) + visuals + content. All lengths are dips.

use crate::geom::*;
use crate::text::Align;
use nova_icons::Icon;

/// Whether an element takes part in the layout (`shown`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Flex,
    /// Out of the layout: it takes no room and draws nothing.
    None,
}

/// How an element is placed: in its parent's flow, or (`Absolute`) by its insets alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Relative,
    Absolute,
}

/// The axis children are laid along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    Column,
}

/// Where children stand across the axis (`align`), or one child (`align-self`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignItems {
    Start,
    Center,
    End,
    Stretch,
    Baseline,
}

/// How children share the room along the axis (`justify`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JustifyContent {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

fn align(a: AlignItems) -> taffy::style::AlignItems {
    use taffy::style::AlignItems as T;
    match a {
        AlignItems::Start => T::FLEX_START,
        AlignItems::Center => T::CENTER,
        AlignItems::End => T::FLEX_END,
        AlignItems::Stretch => T::STRETCH,
        AlignItems::Baseline => T::BASELINE,
    }
}

fn justify(j: JustifyContent) -> taffy::style::JustifyContent {
    use taffy::style::JustifyContent as T;
    match j {
        JustifyContent::Start => T::FLEX_START,
        JustifyContent::Center => T::CENTER,
        JustifyContent::End => T::FLEX_END,
        JustifyContent::SpaceBetween => T::SPACE_BETWEEN,
        JustifyContent::SpaceAround => T::SPACE_AROUND,
        JustifyContent::SpaceEvenly => T::SPACE_EVENLY,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub sigma: f32,
    pub dy: f32,
    pub color: Color,
}

/// A backdrop material (WinUI acrylic): blur standard deviation in dips, the tint (rgb, opacity
/// in a) and the luminosity colour (rgb, opacity in a). Blurred, it carries acrylic's 2 % noise
/// (the frost's grain); with no blur (`sigma` 0) it has none: the backdrop's own pixels pulled to
/// the luminosity and tinted, a flat plate that keeps the colour behind it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    pub sigma: f32,
    pub tint: Color,
    pub lum: Color,
}

impl Material {
    /// AcrylicInAppFillColorDefault, dark: tint #2C2C2C at 0.15, luminosity #2C2C2C at 0.96
    /// (V clamped to 0.125..0.965 leaves it as is), blur 30.
    pub const ACRYLIC_DARK: Material =
        Material { sigma: 30.0, tint: [0.1725, 0.1725, 0.1725, 0.15], lum: [0.1725, 0.1725, 0.1725, 0.96] };
    /// The same colour and opacity with no blur and no noise: a plate at rest over a backdrop
    /// that is already soft (a settings card), where grain over a large fill would only irritate.
    pub const ACRYLIC_FLAT_DARK: Material = Material { sigma: 0.0, ..Material::ACRYLIC_DARK };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Arrow,
    Hand,
    Text,
    Grab,
    Grabbing,
    /// Autoscroll's (a middle press over a list): the up-down arrows.
    ScrollNS,
}

/// Visual properties. `None` colours are not drawn. State colours override the rest colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Visual {
    pub bg: Option<Color>,
    pub bg_hover: Option<Color>,
    pub bg_press: Option<Color>,
    pub bg_lit: Option<Color>,
    pub bg_lit_press: Option<Color>,
    /// While the element (a text field) has the keyboard focus.
    pub bg_focus: Option<Color>,
    pub border_focus: Option<Color>,
    /// A line along the bottom edge, inside the border: the rest colour, and the focus colour
    /// (WinUI TextBox: 1 px ControlStrongStroke at rest, 2 px accent with the focus).
    pub underline: Option<Color>,
    pub underline_focus: Option<Color>,
    pub border: f32,
    pub border_color: Color,
    pub radius: f32,
    /// The plate's corners left square despite `radius`: a mask of `render::primitive::corner`
    /// bits (`corners: top` rounds only the top two). Its fill and border only: a material or a
    /// shadow keeps all four round.
    pub square: u8,
    /// Paint-only plate rectangle [x, y, width, height] in local DIPs. Background, border,
    /// shadow and material use it; layout, content and hit testing keep their own box.
    pub plate: Option<[f32; 4]>,
    pub shadow: Option<Shadow>,
    /// Drawn under the element: what is behind it, blurred and tinted.
    pub material: Option<Material>,
    /// Text / icon colour.
    pub fg: Color,
    pub fg_hover: Option<Color>,
    pub fg_disabled: Option<Color>,
    pub fg_off: Option<Color>,
    pub opacity: f32,
    pub clip: bool,
    pub cursor: Cursor,
    /// Paint-only visibility (layout keeps its place). Layout hiding is `shown: false`.
    pub visible: bool,
    /// Receives pointer events (buttons, links, sliders). Elements with hover/press colours or a
    /// click action are hit targets automatically.
    pub hit: bool,
    /// Own content scale about the centre (children unaffected).
    pub scale: f32,
    /// The box's width only, times `scale`, about the centre: its corners keep their radius
    /// (a switch's knob stretching into a pill). Paint only; content is not stretched.
    pub scale_x: f32,
    /// Underline text while hovered (links).
    pub underline_hover: bool,
    /// A text's karaoke sweep: chars sung (fractional). The chars before it draw in `sweep_fg`,
    /// the rest in the fg; the char at it blends the two across its width. Paint only.
    pub sweep: Option<f32>,
    pub sweep_fg: Color,
    /// A text's units (words or chars), each drawn its own way (`.nui` `u-*`). Paint only.
    pub units: Option<std::rc::Rc<Units>>,
    /// A text turned about its centre, degrees clockwise (±90), after its units are drawn:
    /// `.nui` `text-rotate`. Paint only.
    pub text_rotate: f32,
    /// Where `text_rotate` turns about: dips from the text's centre (`.nui` `text-origin: x, y`;
    /// a line on a wheel turns about the wheel's centre). Paint only.
    pub text_origin: [f32; 2],
}

/// A text's units and how each is drawn (`Visual::units`): its glyphs moved, scaled about the
/// unit's centre, faded and tinted. Chars outside every unit (spaces) draw as the text does.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Units {
    /// Each unit's chars (of the text, `\r` not counted): in order, not overlapping.
    pub chars: Vec<std::ops::Range<u32>>,
    /// One per unit.
    pub fx: Vec<UnitFx>,
}

impl Units {
    /// The unit holding char `c`.
    pub fn of(&self, c: u32) -> Option<usize> {
        let i = self.chars.partition_point(|r| r.end <= c);
        self.chars.get(i).filter(|r| r.start <= c).map(|_| i)
    }
}

/// What a text's units are cut from: words or chars, their timing (spans `(first char, from,
/// to)` in seconds, `text::units`) and the time now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitSrc {
    /// `None`: no units.
    pub words: Option<bool>,
    pub timing: std::rc::Rc<[(u32, f32, f32)]>,
    pub time: f32,
}

/// How one unit is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitFx {
    /// Moved by, in dips.
    pub dx: f32,
    pub dy: f32,
    /// About the unit's centre: across, and down.
    pub scale: f32,
    pub scale_y: f32,
    /// Degrees clockwise, about the unit's centre.
    pub rotate: f32,
    /// Times the text's alpha.
    pub opacity: f32,
    /// In place of the text's colour (the sung part keeps the sweep's).
    pub fg: Option<Color>,
    /// A soft halo under the unit: its radius in dips (whole pixels; 0: none) and colour (its
    /// alpha the halo's strength, times the unit's).
    pub glow: f32,
    pub glow_fg: Color,
    /// A crisp border round the unit's glyphs: its width in dips (whole pixels; 0: none) and
    /// colour (its alpha times the unit's).
    pub outline: f32,
    pub outline_fg: Color,
}

impl UnitFx {
    /// Most units a text animates; past it the text draws plain.
    pub const MAX_UNITS: usize = 512;

    /// Within paint's bounds (`line`: the text's line height in dips): not finite → the
    /// default, moved at most a line, scale 0.5–2, turned ±45°, opacity 0–1, glow 0–16, outline
    /// 0–8. A broken style degrades, never
    /// blows up.
    pub fn clamped(self, line: f32) -> UnitFx {
        let ok = |v: f32, d: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        UnitFx {
            dx: ok(self.dx, 0.0, -line, line),
            dy: ok(self.dy, 0.0, -line, line),
            scale: ok(self.scale, 1.0, 0.5, 2.0),
            scale_y: ok(self.scale_y, 1.0, 0.5, 2.0),
            rotate: ok(self.rotate, 0.0, -45.0, 45.0),
            opacity: ok(self.opacity, 1.0, 0.0, 1.0),
            fg: self.fg.filter(|c| c.iter().all(|v| v.is_finite())),
            glow: ok(self.glow, 0.0, 0.0, 16.0),
            glow_fg: if self.glow_fg.iter().all(|v| v.is_finite()) { self.glow_fg } else { [0.0; 4] },
            outline: ok(self.outline, 0.0, 0.0, 8.0),
            outline_fg: if self.outline_fg.iter().all(|v| v.is_finite()) { self.outline_fg } else { [0.0; 4] },
        }
    }
}

impl Default for UnitFx {
    fn default() -> Self {
        UnitFx {
            dx: 0.0,
            dy: 0.0,
            scale: 1.0,
            scale_y: 1.0,
            rotate: 0.0,
            opacity: 1.0,
            fg: None,
            glow: 0.0,
            glow_fg: [0.0; 4],
            outline: 0.0,
            outline_fg: [0.0; 4],
        }
    }
}

/// How far a text is sung (`Visual::sweep`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sweep {
    /// Chars already sung, fractional inside the one being sung (chars of the text, `\r` not
    /// counted).
    pub at: f32,
    pub lit: Color,
}

impl Default for Visual {
    fn default() -> Self {
        Visual {
            bg: None,
            bg_hover: None,
            bg_press: None,
            bg_lit: None,
            bg_lit_press: None,
            bg_focus: None,
            border_focus: None,
            underline: None,
            underline_focus: None,
            border: 0.0,
            border_color: TRANSPARENT,
            radius: 0.0,
            square: 0,
            plate: None,
            shadow: None,
            material: None,
            fg: [1.0; 4],
            fg_hover: None,
            fg_disabled: None,
            fg_off: None,
            opacity: 1.0,
            clip: false,
            cursor: Cursor::Arrow,
            visible: true,
            hit: false,
            scale: 1.0,
            scale_x: 1.0,
            underline_hover: false,
            sweep: None,
            sweep_fg: [1.0; 4],
            units: None,
            text_rotate: 0.0,
            text_origin: [0.0; 2],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextContent {
    pub text: String,
    pub size: f32,
    pub line: f32,
    pub weight: u16,
    pub align: Align,
    /// 0 = unbounded.
    pub lines: u16,
    /// Wrap to the element width (otherwise one line, ellipsis when too long).
    pub wrap: bool,
}

impl TextContent {
    /// The style its text is measured and laid out in at scale `k` (size and line scaled).
    pub(crate) fn style(&self, k: f32) -> crate::text::TextStyle {
        crate::text::TextStyle { size: self.size * k, line: self.line * k, weight: self.weight }
    }

    /// The most lines it takes: its limit when it wraps, else one.
    pub(crate) fn max_lines(&self) -> u16 {
        if self.wrap {
            self.lines
        } else {
            1
        }
    }
}

impl Default for TextContent {
    fn default() -> Self {
        TextContent {
            text: String::new(),
            size: 14.0,
            line: 20.0,
            weight: 400,
            align: Align::Start,
            lines: 1,
            wrap: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconContent {
    pub icon: Icon,
    pub size: f32,
    /// A second icon knocked out of the first (a filled speech bubble with a quote cut out of
    /// its body), at this percent of its size.
    pub cut: Option<(Icon, u8)>,
    /// An icon was named. A size given alone (a style's `icon-size` on a Button whose mark is a
    /// child, a plugin's Path) holds no icon and draws nothing.
    pub named: bool,
}

/// A vector shape (`vector::Shape`) drawn as a single-colour mark `size` dips square.
#[derive(Clone, Debug, PartialEq)]
pub struct PathContent {
    pub shape: crate::vector::Shape,
    pub size: f32,
    /// The outline's source and box, kept so `view` or `d` set in either order parse once both
    /// are known.
    pub d: Option<std::sync::Arc<str>>,
    pub view: f32,
}

/// An image; `fit_square`: the centred square of a non-square image; `hdr`: on an HDR output, an
/// image with a gain map (`ImageHandle::has_gain_map`) shows its HDR rendition, brighter than SDR
/// white where the picture is (elsewhere, and on SDR outputs, it is the image as it is).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageContent {
    pub image: crate::image::ImageHandle,
    pub opacity: f32,
    pub fit_square: bool,
    pub hdr: bool,
}

/// Two images crossfading: `from` going as `to` comes, `mix` (0 to 1) of the way.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageCrossfade {
    pub from: ImageContent,
    pub to: ImageContent,
    pub mix: f32,
}

/// An image drawn through a registered program (`image::effect`), which reads `params`.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectContent {
    pub effect: crate::image::effect::Effect,
    pub image: crate::image::ImageHandle,
    pub params: [f32; 4],
    pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Content {
    #[default]
    None,
    Text(TextContent),
    Icon(IconContent),
    Path(PathContent),
    Image(ImageContent),
    ImageCrossfade(ImageCrossfade),
    /// An image effect, and optionally a second drawn over it (a crossfade between two).
    Effect(EffectContent, Option<EffectContent>),
}

impl Content {
    /// The images this content draws.
    pub fn images(&self) -> impl Iterator<Item = &crate::image::ImageHandle> {
        let (a, b) = match self {
            Content::Image(a) => (Some(&a.image), None),
            Content::ImageCrossfade(x) => (Some(&x.from.image), Some(&x.to.image)),
            Content::Effect(a, b) => (Some(&a.image), b.as_ref().map(|b| &b.image)),
            _ => (None, None),
        };
        a.into_iter().chain(b)
    }
}

/// Layout, in dips. A thin mirror of the Taffy style so the DSL and the app never see Taffy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub display: Display,
    pub direction: FlexDirection,
    pub position: Position,
    pub width: Len,
    pub height: Len,
    pub min_width: Len,
    pub min_height: Len,
    pub max_width: Len,
    pub max_height: Len,
    pub grow: f32,
    pub shrink: f32,
    pub basis: Len,
    pub gap: f32,
    pub padding: [f32; 4],
    pub margin: [Len; 4],
    pub inset: [Len; 4],
    pub align_items: Option<AlignItems>,
    pub align_self: Option<AlignItems>,
    pub justify: Option<JustifyContent>,
    pub aspect: Option<f32>,
    pub wrap: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    Auto,
    Px(f32),
    Pct(f32),
}

/// A box's sides as `Layout`'s `padding`, `margin` and `inset` hold them, in CSS's order: top,
/// right, bottom, left.
pub(crate) mod side {
    pub const TOP: usize = 0;
    pub const RIGHT: usize = 1;
    pub const BOTTOM: usize = 2;
    pub const LEFT: usize = 3;
}

/// Four sides (`side`) as Taffy's rect of them, each through `f`.
fn taffy_sides<T: Copy, U>(s: [T; 4], f: impl Fn(T) -> U) -> taffy::geometry::Rect<U> {
    taffy::geometry::Rect {
        left: f(s[side::LEFT]),
        right: f(s[side::RIGHT]),
        top: f(s[side::TOP]),
        bottom: f(s[side::BOTTOM]),
    }
}

impl Layout {
    /// Its box's size does not depend on its content: both axes are given in pixels, or (placed
    /// absolutely) pinned between two insets.
    pub fn fixed_size(&self) -> bool {
        let abs = self.position == Position::Absolute;
        let w = matches!(self.width, Len::Px(_))
            || (abs && matches!(self.inset[side::LEFT], Len::Px(_)) && matches!(self.inset[side::RIGHT], Len::Px(_)));
        let h = matches!(self.height, Len::Px(_))
            || (abs && matches!(self.inset[side::TOP], Len::Px(_)) && matches!(self.inset[side::BOTTOM], Len::Px(_)));
        w && h && self.display != Display::None
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            display: Display::Flex,
            direction: FlexDirection::Column,
            position: Position::Relative,
            width: Len::Auto,
            height: Len::Auto,
            min_width: Len::Auto,
            min_height: Len::Auto,
            max_width: Len::Auto,
            max_height: Len::Auto,
            grow: 0.0,
            shrink: 1.0,
            basis: Len::Auto,
            gap: 0.0,
            padding: [0.0; 4],
            margin: [Len::Px(0.0); 4],
            inset: [Len::Auto; 4],
            align_items: None,
            align_self: None,
            justify: None,
            aspect: None,
            wrap: false,
        }
    }
}

fn dim(l: Len) -> taffy::style::Dimension {
    match l {
        Len::Auto => taffy::style::Dimension::auto(),
        Len::Px(v) => taffy::style::Dimension::length(v),
        Len::Pct(v) => taffy::style::Dimension::percent(v / 100.0),
    }
}

fn lpa(l: Len) -> taffy::style::LengthPercentageAuto {
    match l {
        Len::Auto => taffy::style::LengthPercentageAuto::auto(),
        Len::Px(v) => taffy::style::LengthPercentageAuto::length(v),
        Len::Pct(v) => taffy::style::LengthPercentageAuto::percent(v / 100.0),
    }
}

impl Layout {
    pub(crate) fn to_taffy(self) -> taffy::Style {
        use taffy::geometry::Size;
        use taffy::style::LengthPercentage as LP;
        taffy::Style {
            display: match self.display {
                Display::Flex => taffy::style::Display::Flex,
                Display::None => taffy::style::Display::None,
            },
            flex_direction: match self.direction {
                FlexDirection::Row => taffy::style::FlexDirection::Row,
                FlexDirection::Column => taffy::style::FlexDirection::Column,
            },
            position: match self.position {
                Position::Relative => taffy::style::Position::Relative,
                Position::Absolute => taffy::style::Position::Absolute,
            },
            size: Size { width: dim(self.width), height: dim(self.height) },
            min_size: Size { width: lpa(self.min_width), height: lpa(self.min_height) },
            max_size: Size { width: lpa(self.max_width), height: lpa(self.max_height) },
            flex_grow: self.grow,
            flex_shrink: self.shrink,
            flex_basis: dim(self.basis),
            gap: Size { width: LP::length(self.gap), height: LP::length(self.gap) },
            padding: taffy_sides(self.padding, LP::length),
            margin: taffy_sides(self.margin, lpa),
            inset: taffy_sides(self.inset, lpa),
            align_items: self.align_items.map(align),
            align_self: self.align_self.map(align),
            justify_content: self.justify.map(justify),
            aspect_ratio: self.aspect,
            flex_wrap: if self.wrap { taffy::style::FlexWrap::Wrap } else { taffy::style::FlexWrap::NoWrap },
            ..Default::default()
        }
    }
}
