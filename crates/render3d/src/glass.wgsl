// 液态玻璃后处理:在 bevy 的画面上,对指定的圆角矩形区域做模糊 + 边缘折射 + 淡染,
// 其余区域原样透传。作为 bevy 的 FullscreenMaterial 跑在它自己的 ping-pong 纹理上。
//
// 这是 Slint **做不到**的那一步(它没有 backdrop blur,也拿不到自己渲染的像素),
// 而我们能做,因为这块背景是我们自己在 GPU 上画的 —— 见 docs/slint/visual-effects-and-shaders.md。
//
// 顶点阶段由 bevy 的 FullscreenShader 提供,所以这里只有片元入口;模块里必须只有这一个
// @fragment,管线不指定入口名。
//
// 分工:模糊/折射/淡染在这里;边框亮线、内侧厚度、悬浮阴影、指针高光仍由 Slint 的
// GlassCard 画在上面(background 置空,避免淡染叠两遍)。

struct Params {
    // 玻璃矩形,物理像素:xy = 左上角,zw = 宽高
    rect: vec4<f32>,
    // 输出纹理尺寸,物理像素
    tex_size: vec2<f32>,
    // 圆角半径,物理像素
    radius: f32,
    // 模糊半径,物理像素
    blur: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> u: Params;

// 圆角矩形的有符号距离场:内部为负,外部为正,单位是像素。
fn sd_round_rect(p: vec2<f32>, hs: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - hs + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Vogel 螺旋采样的圆盘模糊。32 个点按黄金角均匀铺在圆盘上 —— 比方形核少一半采样
// 就能得到没有方向性伪影的磨砂感。只在玻璃区域内调用,开销与整屏无关。
fn blur_disc(uv: vec2<f32>, radius_px: f32) -> vec4<f32> {
    let texel = 1.0 / u.tex_size;
    var acc = vec4<f32>(0.0);
    for (var i = 0; i < 32; i = i + 1) {
        let fi = f32(i) + 0.5;
        // sqrt 让采样点在圆盘上等面积分布,而不是挤在圆心
        let r = sqrt(fi / 32.0) * radius_px;
        let a = fi * 2.3999632; // 黄金角(弧度)
        let off = vec2<f32>(cos(a), sin(a)) * r * texel;
        acc = acc + textureSampleLevel(src, samp, uv + off, 0.0);
    }
    return acc / 32.0;
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = frag.xy;
    let uv = pix / u.tex_size;

    let center = u.rect.xy + u.rect.zw * 0.5;
    let hs = u.rect.zw * 0.5;
    let d = sd_round_rect(pix - center, hs, u.radius);

    // 玻璃之外:原样透传。这一支覆盖绝大多数像素,所以整体开销极低。
    if (d > 0.0) {
        return textureSampleLevel(src, samp, uv, 0.0);
    }

    // ── 玻璃之内 ──
    // 边缘折射:真玻璃的边是有厚度的,光在那儿被弯折,于是边缘会把背后的像素"吸"过来
    // 放大一圈。用 SDF 的梯度当法线,离边越近位移越大(平方衰减),中心区域则完全不动。
    let e = 1.0;
    let nx = sd_round_rect(pix - center + vec2<f32>(e, 0.0), hs, u.radius)
           - sd_round_rect(pix - center - vec2<f32>(e, 0.0), hs, u.radius);
    let ny = sd_round_rect(pix - center + vec2<f32>(0.0, e), hs, u.radius)
           - sd_round_rect(pix - center - vec2<f32>(0.0, e), hs, u.radius);
    let n = normalize(vec2<f32>(nx, ny) + vec2<f32>(1e-6));

    // EDGE_PX:折射只在离边这么近的一圈内起作用。
    let EDGE_PX = 22.0;
    let edge = clamp(1.0 + d / EDGE_PX, 0.0, 1.0); // d ∈ [-EDGE_PX, 0] → 0..1
    let disp = n * (edge * edge) * 20.0 / u.tex_size;

    var col = blur_disc(uv + disp, u.blur);
    // 淡染:玻璃本体那层白。GlassCard 那边的 background 已置空,不会叠第二遍。
    col = mix(col, vec4<f32>(1.0), 0.10);
    return vec4<f32>(col.rgb, 1.0);
}
