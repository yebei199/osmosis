// 导航侧栏的液态玻璃选中器。整条侧栏背景由本 shader procedural 画(暗底 + 微极光),
// 选中块是两个圆角矩形按 smooth-union 融成的 metaball —— 切 tab 时 lead(快)在前、lag(慢)
// 在后,中间被 smin 拉出一道胶着的颈,像一滴液体从旧槽流到新槽;到位后两者重合成单块。
//
// 与 glass.wgsl 的关键区别:那边折射的是 bevy 画的真实 3D 画面(有源纹理可采样);这里侧栏
// 背后是 Slint 画的(采样不到),所以背景**由本 shader 自己画**,metaball 折射的是这层自绘极光。
// 见 docs/note/slint-bevy-architecture-and-direction.md 第八节的落点讨论。
//
// 独立 wgpu pass:顶点阶段是自带的全屏三角形(不依赖 bevy 的 FullscreenShader),片元里
// 把整条侧栏一次画完。

struct Params {
    // 目标纹理尺寸,物理像素。
    tex_size: vec2<f32>,
    // 三颗球的中心,物理像素:头(快)/尾(慢)/小水滴(最慢)。
    // 追随系数不同,同一个目标位置天然拉开先后(handoff-shaders.md §11)。
    lead: vec2<f32>,
    lag: vec2<f32>,
    drop: vec2<f32>,
    // 头球半尺寸与圆角,物理像素。尾球与小水滴按比例缩(见 fs)。
    half: vec2<f32>,
    radius: f32,
    // smin 的融合半径,物理像素:越大颈越粗、融得越"胶"。
    smooth_k: f32,
    // 深色主题为 1,浅色为 0。
    dark: f32,
    // 移动轴是 x(手机底栏)为 1,是 y(宽版式侧栏)为 0。三球位置由 Rust 侧按轴组好,
    // 这里只用它转置自绘背景的 uv —— 那三团极光是照竖条排的,扁条上不转会挤成一坨。
    horizontal: f32,
    // 尾部填充:把结构体凑成 16 字节的整数倍,满足 uniform 布局对齐(Rust 侧缓冲同为 64 字节)。
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Params;

// 全屏三角形:三个顶点覆盖整个裁剪空间,片元阶段拿到的 @builtin(position) 即物理像素坐标。
@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // (-1,-1) (3,-1) (-1,3) —— 一个比屏幕大的三角形,省掉第二个三角形。
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

// 圆角矩形有符号距离场:内部为负,外部为正,单位像素。
fn sd_round_rect(p: vec2<f32>, hs: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - hs + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// 平滑并集:把两个 SDF 融成带圆润过渡的一个,k 控过渡宽度。metaball 的"胶着颈"就来自这里。
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

// 一团软光斑:中心 c、半径 rad(都在 uv 0..1),平方衰减到边缘全暗。
fn glow(uv: vec2<f32>, c: vec2<f32>, rad: f32) -> f32 {
    let d = length((uv - c) / rad);
    let t = clamp(1.0 - d, 0.0, 1.0);
    return t * t;
}

// 导航条自绘背景:深色底 + 三团极光(与应用 aurora 同调:苔绿/浅葱/薄荷),整体压暗,供玻璃折射透色。
// 光斑沿条的长边分布,短边方向都居中偏移一点,免得死板。入参的 uv 已按移动轴转置过
// (见 fs 里的 buv),所以这里一律按"竖长条"写。
// 色相跟 theme.slint 的 aurora 族同源(docs/adr/0023:绿是唯一强调色,装饰不引入新色相)。
fn base_color(uv: vec2<f32>) -> vec3<f32> {
    if (u.dark < 0.5) {
        // 浅色:亮底,而三团光斑改成**压暗**而不是提亮 —— 在亮底上叠饱和色
        // 只会冲成一片白,极光的结构就没了;减法保留同一处的明暗节奏,
        // 玻璃仍有东西可折射。底色与 slint 那侧的 Theme.aurora-base 同一个值。
        var l = vec3<f32>(0.933, 0.949, 0.910); // #eef2e8
        l -= vec3<f32>(0.12, 0.05, 0.11) * glow(uv, vec2<f32>(0.35, 0.22), 0.55);
        l -= vec3<f32>(0.09, 0.04, 0.09) * glow(uv, vec2<f32>(0.62, 0.55), 0.55);
        l -= vec3<f32>(0.05, 0.02, 0.05) * glow(uv, vec2<f32>(0.45, 0.85), 0.50);
        return l;
    }

    let dark = vec3<f32>(0.043, 0.063, 0.047); // #0b100c,与 Theme.window 同值
    var c = dark;
    c += vec3<f32>(0.247, 0.478, 0.290) * glow(uv, vec2<f32>(0.35, 0.22), 0.55) * 0.22; // 苔绿 #3f7a4a
    c += vec3<f32>(0.525, 0.725, 0.549) * glow(uv, vec2<f32>(0.62, 0.55), 0.55) * 0.20; // 浅葱 #86b98c
    c += vec3<f32>(0.169, 0.361, 0.247) * glow(uv, vec2<f32>(0.45, 0.85), 0.50) * 0.16; // 深林 #2b5c3f
    return c;
}

// 三球联合距离场:头球全尺寸,尾球收一档,小水滴最小(§11 的 52/38×34/20×18
// 按头球比例换算)。行走时三球被追随系数拉开,smin 连出胶着的颈;静止时叠成单块。
fn field(pix: vec2<f32>) -> f32 {
    let d_lead = sd_round_rect(pix - u.lead, u.half, u.radius);
    let d_lag = sd_round_rect(
        pix - u.lag,
        u.half * vec2<f32>(0.73, 0.65),
        u.radius * 0.75);
    let d_drop = sd_round_rect(
        pix - u.drop,
        u.half * vec2<f32>(0.38, 0.35),
        u.radius * 0.5);
    return smin(smin(d_lead, d_lag, u.smooth_k), d_drop, u.smooth_k);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = frag.xy;
    let uv = pix / u.tex_size;
    // 背景采样用的 uv:横条上把两轴对调,光斑仍沿长边排(#70)。
    // 折射位移也在这套坐标里加,所以位移向量同样要对调。
    let flip = u.horizontal > 0.5;
    let buv = select(uv, uv.yx, flip);

    let d = field(pix);

    // 玻璃之外:导航条自绘背景原样。这一支覆盖大多数像素。
    if (d > 0.0) {
        return vec4<f32>(base_color(buv), 1.0);
    }

    // ── 玻璃之内 ──
    // 边缘折射:用 SDF 梯度当法线,离边越近位移越大(平方衰减),把边缘背后的自绘背景"吸"进来。
    let e = 1.0;
    let nx = field(pix + vec2<f32>(e, 0.0)) - field(pix - vec2<f32>(e, 0.0));
    let ny = field(pix + vec2<f32>(0.0, e)) - field(pix - vec2<f32>(0.0, e));
    let n = normalize(vec2<f32>(nx, ny) + vec2<f32>(1e-6));

    let EDGE_PX = 16.0;
    let edge = clamp(1.0 + d / EDGE_PX, 0.0, 1.0); // d ∈ [-EDGE_PX, 0] → 0..1
    let disp = n * (edge * edge) * 14.0 / u.tex_size;

    var col = base_color(buv + select(disp, disp.yx, flip));
    // 玻璃本体淡染:一层薄白,让选中块从背景里浮起来。
    col = mix(col, vec3<f32>(1.0), 0.12);
    // 顶部内侧高光:光从上方来,越靠上缘越亮 —— 液态玻璃最抓眼的那道厚度光边。
    let top = clamp(1.0 + d / 6.0, 0.0, 1.0) * clamp(-n.y, 0.0, 1.0);
    col += vec3<f32>(1.0) * top * 0.25;

    return vec4<f32>(col, 1.0);
}
