//! Image effects: an image drawn through an app-registered program (a custom primitive). The app
//! gives the program twice, as a WGSL fragment function for the GPU backends and as a function
//! for the software renderer, and they must agree within 2/255 (as the built-in primitives do).
//! An element draws it with `Content::Effect`: the image, four parameters the program reads, and
//! an opacity.
//!
//! The WGSL defines `fn <function>(v: VOut) -> vec4<f32>`, added to the renderer's shader (its
//! names must not clash with the renderer's). It sees the renderer's helpers: `v.pos` (the target
//! pixel), `v.p0` (the element's rect: x, y, w, h in target pixels), `v.c0` (the four
//! parameters), `tex_a` (the image) and `bilinear(tex, texel)` (premultiplied, texel centres at
//! +0.5). It returns a premultiplied colour; the renderer multiplies it by the opacity.
//!
//! The WGSL is compiled when the app is built, so no shader compiler ships in it: the app's
//! `build.rs` calls `novaui_shaders::build_effect(name, wgsl_file, function)` (a WGSL error stops
//! the build) and the program names the result `effect_shaders!(name)`.
//!
//! Registration is for the whole process and may happen at any time; a backend makes the
//! programs it has not seen at its next draw.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

static COUNT: AtomicUsize = AtomicUsize::new(0);

/// A registered program. Equal names are one program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Effect(pub(crate) u8);

/// What the software renderer hands the CPU program for one target pixel.
pub struct EffectPixel<'a> {
    /// The pixel centre, from the rect's top-left, in target pixels.
    pub x: f32,
    pub y: f32,
    /// The rect's size in target pixels.
    pub w: f32,
    pub h: f32,
    pub params: [f32; 4],
    /// The image's size in texels.
    pub size: (f32, f32),
    /// Bilinear sample at a texel position (centres at +0.5), premultiplied.
    pub sample: &'a dyn Fn(f32, f32) -> [f32; 4],
}

pub struct EffectProgram {
    pub name: &'static str,
    /// The program for the GPU backends, compiled when the app was built (`effect_shaders!`).
    pub gpu: EffectShaders,
    /// The same program on the CPU.
    pub cpu: fn(&EffectPixel) -> [f32; 4],
}

/// An image effect's GPU program as `novaui_shaders::build_effect` compiled it: the module's
/// HLSL (D3D11) and the SPIR-V of its vertex and fragment entries (Vulkan).
#[derive(Clone, Copy, Debug)]
/// HLSL on Windows, SPIR-V elsewhere (`effect_shaders!`): the other platform's is empty.
pub struct EffectShaders {
    pub hlsl: &'static str,
    pub vs: &'static [u8],
    pub fs: &'static [u8],
}

/// The `EffectShaders` the app's build script compiled as `name`
/// (`novaui_shaders::build_effect(name, …)`), read from its `OUT_DIR`.
#[macro_export]
macro_rules! effect_shaders {
    ($name:literal) => {
        // Each platform's renderer's own: HLSL on Windows (D3D11), SPIR-V elsewhere (Vulkan).
        $crate::image::effect::EffectShaders {
            hlsl: {
                #[cfg(windows)]
                let s: &'static str = include_str!(concat!(env!("OUT_DIR"), "/novaui-effect-", $name, ".hlsl"));
                #[cfg(not(windows))]
                let s: &'static str = "";
                s
            },
            vs: {
                #[cfg(not(windows))]
                let b: &'static [u8] = include_bytes!(concat!(env!("OUT_DIR"), "/novaui-effect-", $name, ".vs.spv"));
                #[cfg(windows)]
                let b: &'static [u8] = &[];
                b
            },
            fs: {
                #[cfg(not(windows))]
                let b: &'static [u8] =
                    include_bytes!(concat!(env!("OUT_DIR"), "/novaui-effect-", $name, ".fs_effect_main.spv"));
                #[cfg(windows)]
                let b: &'static [u8] = &[];
                b
            },
        }
    };
}

/// The registry, locked; a panic while it was held does not stop its use (it is only appended to).
fn table() -> MutexGuard<'static, Vec<&'static EffectProgram>> {
    static T: OnceLock<Mutex<Vec<&'static EffectProgram>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap_or_else(|e| e.into_inner())
}

/// The most programs one process may register.
pub const MAX_EFFECTS: usize = 32;

/// Registers `p` (once per name; again returns the same `Effect`).
pub fn register(p: &'static EffectProgram) -> Effect {
    let mut t = table();
    if let Some(k) = t.iter().position(|q| q.name == p.name) {
        return Effect(k as u8);
    }
    assert!(t.len() < MAX_EFFECTS, "more than {MAX_EFFECTS} image effects");
    t.push(p);
    COUNT.store(t.len(), Ordering::Release);
    Effect((t.len() - 1) as u8)
}

/// How many programs are registered (a backend's cheap check for new ones).
pub(crate) fn count() -> usize {
    COUNT.load(Ordering::Acquire)
}

/// Every program registered so far, in registration order (an `Effect` is its index).
pub(crate) fn programs() -> Vec<&'static EffectProgram> {
    table().clone()
}

/// The programs registered since a backend last looked, with their indices (`Effect`s); `seen`,
/// the count it had, becomes the count now. Empty (no allocation) when none was added.
pub(crate) fn added_since(seen: &mut usize) -> Vec<(usize, &'static EffectProgram)> {
    if count() <= *seen {
        return Vec::new();
    }
    let all = programs();
    let added = all.iter().copied().enumerate().skip(*seen).collect();
    *seen = all.len();
    added
}

pub(crate) fn program(e: Effect) -> Option<&'static EffectProgram> {
    table().get(e.0 as usize).copied()
}
