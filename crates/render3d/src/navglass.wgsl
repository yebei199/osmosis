// 导航指示器:玻璃药丸(docs/design.md「材质」里三处玻璃之一)。整条导航的底由本 shader 一并画:
// 平的窗口底色(Theme.window),药丸画在上面。
//
// 药丸的两条边各跟一个弹簧(lead 快、lag 慢,时长差在 app.slint):走动时药丸先被拉长、
// 再两头一起冲过头落定 —— unicat 那颗球「速度越大越扁」的拉伸,用两条边的时差写出来。
// 静止时两者重合,就是一颗药丸。
//
// 玻璃的层次与 Slint 侧的 Glass token 一一对应:上下渐变的底、1px 边、上沿内高光、
// 向下的一道软影、左上角一团椭圆高光,图标底下一团极淡的强调色。
//
// 独立 wgpu pass:顶点阶段是自带的全屏三角形,片元里把整条导航一次画完。

struct Params {
    // 目标纹理尺寸,物理像素。
    tex_size: vec2<f32>,
    // 药丸两条边各自跟着的中心:lead 快、lag 慢。物理像素。
    lead: vec2<f32>,
    lag: vec2<f32>,
    // 药丸静止时的半尺寸与圆角,物理像素。
    half: vec2<f32>,
    radius: f32,
    // 物理像素 / 逻辑像素。阴影与高光的尺寸按逻辑像素写。
    scale: f32,
    // 深色主题为 1,浅色为 0。
    dark: f32,
    // 移动轴是 x(手机底栏)为 1,是 y(宽版式侧栏)为 0。
    horizontal: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Params;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

// 圆角矩形有符号距离场:内部为负,外部为正,单位像素。
fn sd_round_rect(p: vec2<f32>, hs: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - hs + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// 一团软光:中心 c、半轴 rad,到边缘平方衰减。
fn blob(p: vec2<f32>, c: vec2<f32>, rad: vec2<f32>) -> f32 {
    let t = clamp(1.0 - length((p - c) / rad), 0.0, 1.0);
    return t * t;
}

fn over(dst: vec3<f32>, src: vec3<f32>, a: f32) -> vec3<f32> {
    return mix(dst, src, clamp(a, 0.0, 1.0));
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = frag.xy;
    let dark = u.dark > 0.5;
    // Theme.window:#050806 / #e2e8df。
    let window = select(vec3<f32>(0.886, 0.910, 0.875), vec3<f32>(0.020, 0.031, 0.024), dark);

    // 两条边跨出的那颗药丸:沿移动轴从两颗中心里靠前的那条边到靠后的那条边。
    let axis = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), u.horizontal > 0.5);
    let a = dot(u.lead, axis);
    let b = dot(u.lag, axis);
    let stretch = abs(a - b) * 0.5;
    let center = (u.lead + u.lag) * 0.5;
    let half = u.half + axis * stretch;
    let r = u.radius;
    let d = sd_round_rect(pix - center, half, r);
    let s = u.scale;

    var col = window;
    // 向下的一道软影(Glass.shadow,0 6 16 -6)。
    let sh = sd_round_rect(pix - center - vec2<f32>(0.0, 6.0 * s), half - vec2<f32>(6.0 * s), r);
    let shadow_a = (1.0 - smoothstep(-8.0 * s, 8.0 * s, sh)) * select(0.22, 0.55, dark) * 0.6;
    col = over(col, vec3<f32>(0.0), shadow_a * step(0.0, d));

    if (d < 0.5) {
        let cover = clamp(0.5 - d, 0.0, 1.0);
        // 上下渐变的底:深色 白16%→3.5%,浅色 白95%→55%。
        let v = clamp((pix.y - (center.y - half.y)) / (2.0 * half.y), 0.0, 1.0);
        let top_a = select(0.95, 0.16, dark);
        let bot_a = select(0.55, 0.035, dark);
        var g = over(window, vec3<f32>(1.0), mix(top_a, bot_a, v));
        // 图标底下一团强调色(Glass.tint)。
        let accent = select(vec3<f32>(0.239, 0.490, 0.161), vec3<f32>(0.616, 0.839, 0.447), dark);
        let tint_c = center - vec2<f32>(half.x, 0.0) * (1.0 - u.horizontal) + vec2<f32>(22.0 * s, 0.0) * (1.0 - u.horizontal);
        g = over(g, accent, blob(pix, tint_c, vec2<f32>(36.0 * s)) * select(0.10, 0.16, dark));
        // 左上角的椭圆高光(Glass.spec)。
        let spec_c = center + vec2<f32>(-half.x * 0.35, -half.y);
        g = over(g, vec3<f32>(1.0), blob(pix, spec_c, vec2<f32>(half.x * 0.6, 7.0 * s)) * select(0.6, 0.33, dark));
        // 1px 边(Glass.rim)与上沿内高光(Glass.hi)。
        let rim = 1.0 - smoothstep(0.0, 1.0 * s, -d);
        let rim_col = select(vec3<f32>(0.094, 0.188, 0.110), vec3<f32>(1.0), dark);
        g = over(g, rim_col, rim * select(0.08, 0.07, dark));
        let top_edge = (1.0 - smoothstep(0.0, 1.5 * s, pix.y - (center.y - half.y))) * (1.0 - rim);
        g = over(g, vec3<f32>(1.0), top_edge * select(1.0, 0.24, dark));
        col = over(col, g, cover);
    }
    return vec4<f32>(col, 1.0);
}
