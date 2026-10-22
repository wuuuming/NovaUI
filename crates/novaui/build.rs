//! The renderer's shaders, compiled now rather than when a program starts (novaui-shaders):
//! `prims.hlsl` and one `prims.<entry>.spv` per entry point in `OUT_DIR`, with the table of the
//! latter (`prims_spirv.rs`); and the docs example's image effect, as an app compiles its own.

fn main() {
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let dir = std::path::Path::new(&out);
    let c = novaui_shaders::renderer().unwrap_or_else(|e| panic!("prims.wgsl:\n{e}"));
    novaui_shaders::write(&c, dir, "prims").expect("write the renderer's shaders");
    let mut table = String::from(
        "/// The renderer's SPIR-V, one module per entry point.\npub const SPIRV: &[(&str, &[u8])] = &[\n",
    );
    for (entry, _) in &c.spirv {
        table.push_str(&format!(
            "    ({entry:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/prims.{entry}.spv\"))),\n"
        ));
    }
    table.push_str("];\n");
    std::fs::write(dir.join("prims_spirv.rs"), table).expect("write prims_spirv.rs");
    novaui_shaders::build_effect("docs_duotone", "examples/docs_custom.wgsl", "docs_duotone");
    println!("cargo:rerun-if-changed=build.rs");
}
