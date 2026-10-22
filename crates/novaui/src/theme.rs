//! Theme tokens: the colours, metrics, type ramp, shadows and materials the DSL resolves bare
//! identifiers to (after the sheet's own `token` definitions). The app installs a `Theme`
//! (`install`) before its sheets are applied; until then, and as a starting point,
//! `Theme::winui_dark` is in force: the WinUI 3 dark theme resources (Microsoft Learn /
//! microsoft-ui-xaml `Common_themeresources_any.xaml`), the type ramp and the acrylic material.
//!
//! `Theme::material3_dark` / `material3_light` are the Material 3 baseline (seed `#6750A4`) under
//! the M3 system token names (`md-sys-color-primary`, `md-sys-typescale-body-large`,
//! `md-sys-shape-corner-medium`, `md-sys-elevation-level2`, `md-sys-state-hover-state-layer-opacity`).
//! What novaui does not draw is left out or approximated: the typescale has no letter spacing, an
//! elevation is one shadow (M3 draws a key and an ambient shadow), and there is no dynamic colour
//! from a seed; a state layer is the app's own hover/press colour.

use crate::geom::*;
use crate::style::Material;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tok {
    Color(Color),
    Num(f32),
    /// size, line height, weight
    Type(f32, f32, u16),
    /// sigma, dy, colour
    Shadow(f32, f32, Color),
    /// A backdrop material (`material: <name>`).
    Material(Material),
}

/// A set of named tokens.
#[derive(Clone, Debug, Default)]
pub struct Theme {
    tokens: HashMap<String, Tok>,
}

impl Theme {
    /// No tokens.
    pub fn empty() -> Self {
        Theme::default()
    }

    /// The WinUI 3 dark preset.
    pub fn winui_dark() -> Self {
        use Tok::*;
        let table: &[(&str, Tok)] = &[
            // Text (dark)
            ("text-primary", Color(argb(0xFFFFFFFF))),
            ("text-secondary", Color(argb(0xC5FFFFFF))),
            ("text-tertiary", Color(argb(0x87FFFFFF))),
            ("text-disabled", Color(argb(0x5DFFFFFF))),
            ("accent-text", Color(argb(0xFF99EBFF))),
            // Fills (dark)
            ("control-fill", Color(argb(0x0FFFFFFF))),
            ("control-fill-secondary", Color(argb(0x15FFFFFF))),
            ("control-fill-tertiary", Color(argb(0x08FFFFFF))),
            ("subtle-secondary", Color(argb(0x0FFFFFFF))),
            ("subtle-tertiary", Color(argb(0x0AFFFFFF))),
            ("card-fill", Color(argb(0x0DFFFFFF))),
            ("layer-fill", Color(argb(0x4C3A3A3A))),
            ("solid-base", Color(argb(0xFF202020))),
            ("solid-secondary", Color(argb(0xFF1C1C1C))),
            ("solid-tertiary", Color(argb(0xFF282828))),
            ("flyout-fill", Color(argb(0xFF2C2C2C))),
            ("stroke-control", Color(argb(0x12FFFFFF))),
            ("stroke-card", Color(argb(0x19000000))),
            ("stroke-surface", Color(argb(0x66757575))),
            ("stroke-flyout", Color(argb(0x33000000))),
            ("divider", Color(argb(0x15FFFFFF))),
            ("accent", Color(argb(0xFF60CDFF))),
            // The keyboard focus visual (FocusStrokeColorOuter / Inner).
            ("focus-stroke-outer", Color(argb(0xFFFFFFFF))),
            ("focus-stroke-inner", Color(argb(0xB3000000))),
            ("caption-close-hover", Color(argb(0xFFC42B1C))),
            ("caption-close-press", Color(argb(0xE6C42B1C))),
            ("white", Color(argb(0xFFFFFFFF))),
            ("black", Color(argb(0xFF000000))),
            ("transparent", Color(TRANSPARENT)),
            // Metrics
            ("radius-control", Num(4.0)),
            ("radius-overlay", Num(8.0)),
            ("control-height", Num(32.0)),
            ("space-xxs", Num(2.0)),
            ("space-xs", Num(4.0)),
            ("space-s", Num(8.0)),
            ("space-m", Num(12.0)),
            ("space-l", Num(16.0)),
            ("space-xl", Num(20.0)),
            ("space-xxl", Num(24.0)),
            ("space-xxxl", Num(32.0)),
            ("titlebar-height", Num(32.0)),
            ("caption-button-width", Num(46.0)),
            // Type ramp
            ("caption", Type(12.0, 16.0, 400)),
            ("body", Type(14.0, 20.0, 400)),
            ("body-strong", Type(14.0, 20.0, 600)),
            ("body-large", Type(18.0, 24.0, 400)),
            ("subtitle", Type(20.0, 28.0, 600)),
            ("title", Type(28.0, 36.0, 600)),
            ("title-large", Type(40.0, 52.0, 600)),
            ("display", Type(68.0, 92.0, 600)),
            // Shadows (WinUI ThemeShadow approximations: Flyout 32 dp elevation, Dialog 128 dp)
            ("shadow-flyout", Shadow(12.0, 8.0, argb(0x47000000))),
            ("shadow-dialog", Shadow(24.0, 16.0, argb(0x5C000000))),
            ("shadow-card", Shadow(4.0, 2.0, argb(0x24000000))),
            // Materials
            ("acrylic", Material(crate::style::Material::ACRYLIC_DARK)),
            ("acrylic-flat", Material(crate::style::Material::ACRYLIC_FLAT_DARK)),
        ];
        Theme { tokens: table.iter().map(|&(k, t)| (k.to_string(), t)).collect() }
    }

    /// The Material 3 baseline dark scheme, with the M3 typescale, shapes, elevations and state
    /// layer opacities. `accent` (the text selection's colour) is the scheme's primary.
    pub fn material3_dark() -> Self {
        Theme::material3(&[
            ("primary", 0xD0BCFF),
            ("on-primary", 0x381E72),
            ("primary-container", 0x4F378B),
            ("on-primary-container", 0xEADDFF),
            ("secondary", 0xCCC2DC),
            ("on-secondary", 0x332D41),
            ("secondary-container", 0x4A4458),
            ("on-secondary-container", 0xE8DEF8),
            ("tertiary", 0xEFB8C8),
            ("on-tertiary", 0x492532),
            ("tertiary-container", 0x633B48),
            ("on-tertiary-container", 0xFFD8E4),
            ("error", 0xF2B8B5),
            ("on-error", 0x601410),
            ("error-container", 0x8C1D18),
            ("on-error-container", 0xF9DEDC),
            ("background", 0x141218),
            ("on-background", 0xE6E0E9),
            ("surface", 0x141218),
            ("on-surface", 0xE6E0E9),
            ("surface-variant", 0x49454F),
            ("on-surface-variant", 0xCAC4D0),
            ("surface-bright", 0x3B383E),
            ("surface-dim", 0x141218),
            ("surface-container-lowest", 0x0F0D13),
            ("surface-container-low", 0x1D1B20),
            ("surface-container", 0x211F26),
            ("surface-container-high", 0x2B2930),
            ("surface-container-highest", 0x36343B),
            ("inverse-surface", 0xE6E0E9),
            ("inverse-on-surface", 0x322F35),
            ("inverse-primary", 0x6750A4),
            ("outline", 0x938F99),
            ("outline-variant", 0x49454F),
            ("shadow", 0x000000),
            ("scrim", 0x000000),
        ])
    }

    /// The Material 3 baseline light scheme; otherwise as `material3_dark`.
    pub fn material3_light() -> Self {
        Theme::material3(&[
            ("primary", 0x6750A4),
            ("on-primary", 0xFFFFFF),
            ("primary-container", 0xEADDFF),
            ("on-primary-container", 0x4F378B),
            ("secondary", 0x625B71),
            ("on-secondary", 0xFFFFFF),
            ("secondary-container", 0xE8DEF8),
            ("on-secondary-container", 0x4A4458),
            ("tertiary", 0x7D5260),
            ("on-tertiary", 0xFFFFFF),
            ("tertiary-container", 0xFFD8E4),
            ("on-tertiary-container", 0x633B48),
            ("error", 0xB3261E),
            ("on-error", 0xFFFFFF),
            ("error-container", 0xF9DEDC),
            ("on-error-container", 0x8C1D18),
            ("background", 0xFEF7FF),
            ("on-background", 0x1D1B20),
            ("surface", 0xFEF7FF),
            ("on-surface", 0x1D1B20),
            ("surface-variant", 0xE7E0EC),
            ("on-surface-variant", 0x49454F),
            ("surface-bright", 0xFEF7FF),
            ("surface-dim", 0xDED8E1),
            ("surface-container-lowest", 0xFFFFFF),
            ("surface-container-low", 0xF7F2FA),
            ("surface-container", 0xF3EDF7),
            ("surface-container-high", 0xECE6F0),
            ("surface-container-highest", 0xE6E0E9),
            ("inverse-surface", 0x322F35),
            ("inverse-on-surface", 0xF5EFF7),
            ("inverse-primary", 0xD0BCFF),
            ("outline", 0x79747E),
            ("outline-variant", 0xCAC4D0),
            ("shadow", 0x000000),
            ("scrim", 0x000000),
        ])
    }

    /// A Material 3 theme from a scheme's colour roles (`role`, `0xRRGGBB`), each set as
    /// `md-sys-color-<role>`, and the scheme-independent tokens.
    fn material3(scheme: &[(&str, u32)]) -> Self {
        use Tok::*;
        let mut t = Theme::empty();
        for &(role, rgb) in scheme {
            t.set(&format!("md-sys-color-{role}"), Color(argb(0xFF00_0000 | rgb)));
        }
        if let Some(primary) = t.get("md-sys-color-primary") {
            t.set("accent", primary);
        }
        // The focus indicator: a ring in the secondary colour, no inner one.
        if let Some(secondary) = t.get("md-sys-color-secondary") {
            t.set("focus-stroke-outer", secondary);
        }
        t.set("focus-stroke-inner", Color(TRANSPARENT));
        // Typescale: size, line height, weight (M3's letter spacing is not drawn).
        let typescale: &[(&str, f32, f32, u16)] = &[
            ("display-large", 57.0, 64.0, 400),
            ("display-medium", 45.0, 52.0, 400),
            ("display-small", 36.0, 44.0, 400),
            ("headline-large", 32.0, 40.0, 400),
            ("headline-medium", 28.0, 36.0, 400),
            ("headline-small", 24.0, 32.0, 400),
            ("title-large", 22.0, 28.0, 400),
            ("title-medium", 16.0, 24.0, 500),
            ("title-small", 14.0, 20.0, 500),
            ("body-large", 16.0, 24.0, 400),
            ("body-medium", 14.0, 20.0, 400),
            ("body-small", 12.0, 16.0, 400),
            ("label-large", 14.0, 20.0, 500),
            ("label-medium", 12.0, 16.0, 500),
            ("label-small", 11.0, 16.0, 500),
        ];
        for &(name, size, line, weight) in typescale {
            t.set(&format!("md-sys-typescale-{name}"), Type(size, line, weight));
        }
        // Shape: corner radii; `full` is clamped to half the shorter side when drawn.
        let corners: &[(&str, f32)] = &[
            ("none", 0.0),
            ("extra-small", 4.0),
            ("small", 8.0),
            ("medium", 12.0),
            ("large", 16.0),
            ("extra-large", 28.0),
            ("full", 9999.0),
        ];
        for &(name, r) in corners {
            t.set(&format!("md-sys-shape-corner-{name}"), Num(r));
        }
        // Elevation 0, 1, 3, 6, 8, 12 dp, each as one shadow (blur sigma, offset, colour).
        let levels = [
            (0.0, 0.0, 0x0000_0000),
            (1.5, 1.0, 0x4D00_0000),
            (2.5, 2.0, 0x4D00_0000),
            (4.0, 4.0, 0x4D00_0000),
            (5.0, 6.0, 0x4D00_0000),
            (7.0, 8.0, 0x4D00_0000),
        ];
        for (i, &(sigma, dy, c)) in levels.iter().enumerate() {
            t.set(&format!("md-sys-elevation-level{i}"), Shadow(sigma, dy, argb(c)));
        }
        // State layers: the opacity of the content colour laid over a container.
        let states = [("hover", 0.08), ("focus", 0.10), ("pressed", 0.10), ("dragged", 0.16)];
        for (name, o) in states {
            t.set(&format!("md-sys-state-{name}-state-layer-opacity"), Num(o));
        }
        t
    }

    /// Sets (or replaces) `name`.
    pub fn set(&mut self, name: &str, tok: Tok) -> &mut Self {
        self.tokens.insert(name.to_string(), tok);
        self
    }

    pub fn get(&self, name: &str) -> Option<Tok> {
        self.tokens.get(name).copied()
    }
}

fn current() -> &'static RwLock<Arc<Theme>> {
    static T: OnceLock<RwLock<Arc<Theme>>> = OnceLock::new();
    T.get_or_init(|| RwLock::new(Arc::new(Theme::winui_dark())))
}

/// Makes `t` the theme the DSL resolves names in, from the next sheet applied on.
pub fn install(t: Theme) {
    *current().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(t);
}

/// The installed theme's token `name`.
pub fn token(name: &str) -> Option<Tok> {
    current().read().unwrap_or_else(|e| e.into_inner()).get(name)
}

/// The installed theme's colour token `name`.
pub fn color(name: &str) -> Option<Color> {
    match token(name)? {
        Tok::Color(c) => Some(c),
        _ => None,
    }
}
