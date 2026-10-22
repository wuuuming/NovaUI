// The duotone of docs_custom.rs, for the GPU backends: compiled when novaui is built
// (build.rs, novaui-shaders), as an app's build script compiles its effects.
fn docs_duotone(v: VOut) -> vec4<f32> {
    let n = vec2<f32>(textureDimensions(tex_a, 0));
    let p = floor(v.pos.xy) + vec2<f32>(0.5, 0.5);
    let c = bilinear(tex_a, (p - v.p0.xy) / v.p0.zw * n);
    let l = dot(c.rgb, vec3<f32>(0.299, 0.587, 0.114)) / max(c.a, 0.0001);
    let tone = mix(vec3<f32>(0.10, 0.06, 0.32), vec3<f32>(1.0, 0.86, 0.45), l) * c.a;
    return vec4<f32>(mix(c.rgb, tone, v.c0.x), c.a);
}
