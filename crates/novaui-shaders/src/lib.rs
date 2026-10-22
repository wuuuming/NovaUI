//! NovaUI's GPU programs, compiled when a program is built: the renderer's WGSL and an app's image
//! effects, translated by naga to HLSL (D3D11, shader model 5.0; D3D11 compiles it at start with
//! the system's compiler) and SPIR-V (Vulkan 1.0). NovaUI's build script calls `renderer`; an app
//! with image effects calls `build_effect` from its own `build.rs` and names the result with
//! `novaui::effect_shaders!` (novaui `image::effect`). A WGSL error stops the build with naga's
//! message.

use std::path::Path;

/// The renderer's shader: one vertex entry `vs`, one fragment entry per primitive kind. The math
/// mirrors the software renderer expression by expression.
pub const PRIMS: &str = include_str!("prims.wgsl");

/// The fragment entry of an image effect's module.
pub const EFFECT_ENTRY: &str = "fs_effect_main";

/// A module compiled for both GPU backends.
pub struct Compiled {
    /// HLSL holding every entry point.
    pub hlsl: String,
    /// SPIR-V per entry point kept, in the module's order.
    pub spirv: Vec<(String, Vec<u32>)>,
}

/// Compiles `src`; SPIR-V for the entries `keep` names (every entry when `None`).
fn compile(src: &str, keep: Option<&[&str]>) -> Result<Compiled, String> {
    use naga::back::{hlsl, spv};
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    let m = naga::front::wgsl::parse_str(src).map_err(|e| e.emit_to_string(src))?;
    let info =
        Validator::new(ValidationFlags::all(), Capabilities::all()).validate(&m).map_err(|e| e.emit_to_string(src))?;
    let opts = hlsl::Options { shader_model: hlsl::ShaderModel::V5_0, ..Default::default() };
    let mut text = String::new();
    hlsl::Writer::new(&mut text, &opts).write(&m, &info, None).map_err(|e| format!("hlsl: {e}"))?;
    let spv_opts =
        spv::Options { lang_version: (1, 0), flags: spv::WriterFlags::ADJUST_COORDINATE_SPACE, ..Default::default() };
    let mut spirv = Vec::new();
    for ep in m.entry_points.iter().filter(|ep| keep.is_none_or(|k| k.contains(&ep.name.as_str()))) {
        let po = spv::PipelineOptions { shader_stage: ep.stage, entry_point: ep.name.clone() };
        let words = spv::write_vec(&m, &info, &spv_opts, Some(&po)).map_err(|e| format!("spv {}: {e}", ep.name))?;
        spirv.push((ep.name.clone(), words));
    }
    Ok(Compiled { hlsl: text, spirv })
}

/// The renderer's own module.
pub fn renderer() -> Result<Compiled, String> {
    compile(PRIMS, None)
}

/// The module of an image effect: the renderer's source with `wgsl` (defining
/// `fn <function>(v: VOut) -> vec4<f32>`) and the entry `EFFECT_ENTRY`, which returns the
/// function's colour times the draw's opacity.
pub fn effect_source(wgsl: &str, function: &str) -> String {
    format!(
        "{PRIMS}\n{wgsl}\n@fragment\nfn {EFFECT_ENTRY}(v: VOut) -> @location(0) vec4<f32> {{\n    return {function}(v) * v.k.w;\n}}\n"
    )
}

/// An image effect compiled: the HLSL, and the SPIR-V of `vs` and `EFFECT_ENTRY`.
pub fn effect(wgsl: &str, function: &str) -> Result<Compiled, String> {
    compile(&effect_source(wgsl, function), Some(&["vs", EFFECT_ENTRY]))
}

/// Writes `c` into `dir` as `<stem>.hlsl` and `<stem>.<entry>.spv` (little-endian words).
pub fn write(c: &Compiled, dir: &Path, stem: &str) -> std::io::Result<()> {
    std::fs::write(dir.join(format!("{stem}.hlsl")), &c.hlsl)?;
    for (entry, words) in &c.spirv {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        std::fs::write(dir.join(format!("{stem}.{entry}.spv")), bytes)?;
    }
    Ok(())
}

/// For an app's `build.rs`: compiles the image effect in `wgsl_file` (relative to the package)
/// whose function is `function`, into `OUT_DIR` for `novaui::effect_shaders!(name)`.
///
/// # Panics
///
/// Outside a build script (no `OUT_DIR`), and when the WGSL does not compile: the build stops
/// with naga's message.
pub fn build_effect(name: &str, wgsl_file: &str, function: &str) {
    println!("cargo:rerun-if-changed={wgsl_file}");
    let src = std::fs::read_to_string(wgsl_file).unwrap_or_else(|e| panic!("{wgsl_file}: {e}"));
    let out = std::env::var("OUT_DIR").unwrap_or_else(|_| panic!("build_effect is for a build script (no OUT_DIR)"));
    let c = effect(&src, function).unwrap_or_else(|e| panic!("image effect {name} ({wgsl_file}):\n{e}"));
    write(&c, Path::new(&out), &format!("novaui-effect-{name}")).unwrap_or_else(|e| panic!("{out}: {e}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_renderer_and_an_effect_compile_for_both_backends() {
        let r = renderer().unwrap();
        assert!(r.spirv.iter().any(|(e, _)| e == "vs"));
        assert!(r.spirv.len() > 2 && r.hlsl.contains("fs_glyph"));
        let e = effect("fn tint(v: VOut) -> vec4<f32> { return v.c0; }", "tint").unwrap();
        let names: Vec<&str> = e.spirv.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["vs", EFFECT_ENTRY]);
        assert!(effect("fn broken(v: VOut) -> vec4<f32> { return 1; }", "broken").is_err());
    }
}
