// 播放页封面点云:六档视觉预设的顶点位移 + 软边圆点。
//
// 逐条照抄 Mineradio `02-visual/00-pointer-cover-particles.js` 顶点着色器里按
// `uPreset` 分的那几段分支(见 crates/render3d/src/cloud.rs 的模块注释)。
// 十几万颗粒子烘在同一份顶点缓冲里,每颗四个顶点拼一个正对相机的方片;
// 预设之间**复用同一份几何** —— 每档读的都是同一批 (格点 uv, 角偏移, 随机数),
// 只是把它们摆到不同的地方。运动学全在这里,CPU 每帧只换 uniform。
//
// 原版的几何是照它那块 4.8 的平面、6.6 的相机半径调的。铺满画面的两档(封面、星河)
// 按平面倍数搬,物体类三档(滚筒、星球、唱片)按相机距离搬 —— 后者是被取景的一个
// 物体,按平面倍数放大会大到糊在镜头上。

#import bevy_pbr::mesh_functions
#import bevy_pbr::mesh_view_bindings::view

struct CloudParams {
    // 播放页时钟,秒。门关即冻结。
    time: f32,
    // 三段电平,各自 0..=1。
    bass: f32,
    mid: f32,
    treble: f32,
    // 律动强度,同原版 uIntensity。
    intensity: f32,
    // 位移幅度的整体倍数:平面比原版放大了多少。
    plane_scale: f32,
    // 物体类预设(滚筒/星球/唱片)的尺度倍数,按相机距离对齐。
    object_scale: f32,
    // 当前预设的下标:0 封面、1 滚筒、2 星球、3 虚空、4 唱片、5 星河。
    preset: u32,
    // 封面纹理有没有内容。0 时走默认渐变色,点云在没有封面时不消失。
    has_cover: f32,
    // 片元 alpha 的丢弃门槛。桌面走软边(近 0),安卓走硬边(0.5),
    // 理由见 docs/adr/0012:Adreno 上半透明小元素整片不显示。
    alpha_cutoff: f32,
    // 圆点半径,世界单位。按格距定而不是按像素定,视口一变点与缝的比例才不跑。
    point_radius: f32,
    // 新旧封面的混合进度:0 = 全旧,1 = 全新。
    color_mix: f32,
    // 换歌脉冲强度:1 = 刚换,0 = 已归位。
    burst: f32,
    // 还活着的涟漪路数。着色器靠它提前跳出循环,不必每帧空转十二遍。
    ripple_count: u32,
    // 涟漪的半径与幅度按平面放大的倍数同比放大。
    ripple_scale: f32,
    // 涟漪表:每路 (x, y, 年龄, 强度)。
    ripple_slots: array<vec4<f32>, 12>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: CloudParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var cover_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var cover_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var prev_cover_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var prev_cover_sampler: sampler;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    // 格点在点云平面上的基准位。
    @location(0) position: vec3<f32>,
    // 立方体的角偏移,三分量各 ±1。借的是法线属性位(见 build_cloud_mesh)。
    @location(1) corner: vec3<f32>,
    // 封面纹理的采样坐标。
    @location(2) uv: vec2<f32>,
    // (面法线 xyz, 逐粒子随机数)。借的是切线属性位。
    @location(3) tangent: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    // 整颗粒子的透明度倍数。虚空档靠它整层隐身,星河档靠它分出明暗。
    @location(1) alpha: f32,
}

// 一颗粒子被预设摆到哪、用哪块封面上色、亮多少、透多少。
//
// 各档返回同一个结构,顶点主函数只管把它投影出去 —— 否则每加一档都要在主函数里
// 再插一段分支,几档之后就没人看得懂哪个变量是谁写的。
struct Placement {
    // 位置,**最终尺度**。铺满画面的两档(封面/星河)自己就在最终尺度上,
    // 物体类三档(滚筒/星球/唱片)在自己那段里乘过 object_scale。
    local: vec3<f32>,
    // 封面采样坐标。滚筒与唱片会改它(沿轴流动 / 圆形裁切)。
    uv: vec2<f32>,
    // 颜色的额外乘子:纵深淡出、极光染色之类。
    tint: vec3<f32>,
    // 透明度倍数。
    alpha: f32,
    // 亮度加成,叠到节拍提亮上。
    glow: f32,
    // 采多少封面色。虚空档给 0,星河档只留一点底色。
    cover: f32,
}

const PI: f32 = 3.14159265359;

// 涟漪表的路数,与 cloud.rs 的 RIPPLE_SLOTS 手工对齐。
const RIPPLE_SLOTS: u32 = 12u;

// 给立方体分面用的固定光向(世界空间,已归一)。偏上偏右前方 —— 顶面最亮、
// 正面居中、背面与底面压暗,方块的体积就出来了。不是物理光照,只是分面。
const CUBE_LIGHT: vec3<f32> = vec3<f32>(0.4045, 0.8090, 0.4264);

// ── simplex 噪声(照搬原版顶点着色器里那一份)────────────────────────────

fn mod289(x: vec3<f32>) -> vec3<f32> {
    return x - floor(x * (1.0 / 289.0)) * 289.0;
}

fn mod289v(x: vec4<f32>) -> vec4<f32> {
    return x - floor(x * (1.0 / 289.0)) * 289.0;
}

fn perm(x: vec4<f32>) -> vec4<f32> {
    return mod289v(((x * 34.0) + 1.0) * x);
}

fn snoise(v: vec3<f32>) -> f32 {
    let C = vec2<f32>(1.0 / 6.0, 1.0 / 3.0);
    let D = vec4<f32>(0.0, 0.5, 1.0, 2.0);
    var i = floor(v + dot(v, C.yyy));
    let x0 = v - i + dot(i, C.xxx);
    let g = step(x0.yzx, x0.xyz);
    let l = 1.0 - g;
    let i1 = min(g.xyz, l.zxy);
    let i2 = max(g.xyz, l.zxy);
    let x1 = x0 - i1 + C.xxx;
    let x2 = x0 - i2 + C.yyy;
    let x3 = x0 - D.yyy;
    i = mod289(i);
    let p = perm(perm(perm(
        i.z + vec4<f32>(0.0, i1.z, i2.z, 1.0)) +
        i.y + vec4<f32>(0.0, i1.y, i2.y, 1.0)) +
        i.x + vec4<f32>(0.0, i1.x, i2.x, 1.0));
    let n_ = 0.142857142857;
    let ns = n_ * D.wyz - D.xzx;
    let j = p - 49.0 * floor(p * ns.z * ns.z);
    let x_ = floor(j * ns.z);
    let y_ = floor(j - 7.0 * x_);
    let x = x_ * ns.x + ns.yyyy;
    let y = y_ * ns.x + ns.yyyy;
    let h = 1.0 - abs(x) - abs(y);
    let b0 = vec4<f32>(x.xy, y.xy);
    let b1 = vec4<f32>(x.zw, y.zw);
    let s0 = floor(b0) * 2.0 + 1.0;
    let s1 = floor(b1) * 2.0 + 1.0;
    let sh = -step(h, vec4<f32>(0.0));
    let a0 = b0.xzyw + s0.xzyw * sh.xxyy;
    let a1 = b1.xzyw + s1.xzyw * sh.zzww;
    var p0 = vec3<f32>(a0.xy, h.x);
    var p1 = vec3<f32>(a0.zw, h.y);
    var p2 = vec3<f32>(a1.xy, h.z);
    var p3 = vec3<f32>(a1.zw, h.w);
    let norm = inverseSqrt(vec4<f32>(
        dot(p0, p0), dot(p1, p1), dot(p2, p2), dot(p3, p3)));
    p0 *= norm.x;
    p1 *= norm.y;
    p2 *= norm.z;
    p3 *= norm.w;
    var m = max(0.6 - vec4<f32>(
        dot(x0, x0), dot(x1, x1), dot(x2, x2), dot(x3, x3)), vec4<f32>(0.0));
    m = m * m;
    return 42.0 * dot(m * m, vec4<f32>(
        dot(p0, x0), dot(p1, x1), dot(p2, x2), dot(p3, x3)));
}

// 下标 → [0,1) 的散列,照搬原版的 `hash11`。
fn hash11(p: f32) -> f32 {
    return fract(sin(p * 127.1) * 43758.5453123);
}

// 某点上所有涟漪叠加出的纵深位移。照抄原版的 `rippleSumAt`:一个中心隆起的
// 高斯包(bulge)加一圈往外走的环(ring),各自随年龄展宽并淡出。
fn ripple_sum_at(p: vec2<f32>) -> f32 {
    var total = 0.0;
    let scale = params.ripple_scale;
    for (var i = 0u; i < RIPPLE_SLOTS; i = i + 1u) {
        if i >= params.ripple_count {
            break;
        }
        let data = params.ripple_slots[i];
        let age = data.z;
        let strength = data.w;
        if strength < 0.005 || age < 0.0 || age > 2.0 {
            continue;
        }
        let dist = length(p - data.xy);
        let life = age / 2.0;
        let fade_in = smoothstep(0.0, 0.06, age);
        let fade_out = 1.0 - smoothstep(0.7, 1.0, life);
        let env = fade_in * fade_out;
        let bulge_w = (0.55 + age * 0.80) * scale;
        let bulge = exp(-dist * dist / (2.0 * bulge_w * bulge_w))
            * (1.0 - smoothstep(0.0, 0.55, life));
        let wave_r = age * 2.10 * scale;
        let ring_w = (0.40 + age * 0.22) * scale;
        let ring = exp(-pow((dist - wave_r) / ring_w, 2.0));
        total = total + (bulge * 2.4 + ring * 1.30) * env * strength * scale;
    }
    return total;
}

// 一档静止的默认摆位,各预设在它上面改。
fn placement_default(uv: vec2<f32>) -> Placement {
    return Placement(
        vec3<f32>(0.0), uv, vec3<f32>(1.0), 1.0, 0.0, 1.0);
}

// ── Preset 0:封面(SILK)──────────────────────────────────────────────
//
// xy 停在格点上,起伏全走 z —— 从正面看是一张随音乐抖动的封面,侧面看才知道它
// 是有厚度的一层。涟漪只叠在这一档上(原版也只有 SILK 吃鼠标)。
fn place_cover(
    base: vec3<f32>, uv: vec2<f32>, rand: f32, t: f32, k: f32, s: f32,
) -> Placement {
    var out = placement_default(uv);
    // base 已经是放大后的尺度,噪声取样也就直接用它;只有 z 的幅度要乘 s。
    let mid_n = snoise(vec3<f32>(base.x * 1.4, base.y * 1.4, t * 0.55)) * 0.6
        + snoise(vec3<f32>(base.x * 2.8 + 5.0, base.y * 2.8 - 3.0, t * 0.85)) * 0.4;
    let mid_mask = 0.55 + 0.45 * snoise(
        vec3<f32>(base.x * 0.4, base.y * 0.4, t * 0.18));
    let mid_disp = mid_n * params.mid * 0.55 * mid_mask * k * s;
    let treble_jitter = snoise(vec3<f32>(
        base.x * 6.5, base.y * 6.5, t * 3.5 + rand * 4.0))
        * params.treble * 0.18 * k * s;
    let bass_breath = snoise(vec3<f32>(
        base.x * 0.35, base.y * 0.35, t * 0.4)) * params.bass * 0.42 * k * s;
    let ripple_z = ripple_sum_at(base.xy) * 1.30;

    out.local = vec3<f32>(
        base.x, base.y,
        ripple_z + mid_disp + treble_jitter + bass_breath);
    out.glow = clamp(abs(ripple_z), 0.0, 1.0) * 0.55;
    return out;
}

// ── Preset 1:滚筒(TUNNEL)────────────────────────────────────────────
//
// uv.x 绕一圈成管壁、uv.y 沿轴流动,低频收管径,整管缓慢自旋。封面沿轴滚过去,
// 所以采样的 v 用的是流动后的坐标而不是原格点。
fn place_tunnel(uv: vec2<f32>, t: f32, k: f32) -> Placement {
    var out = placement_default(uv);
    let spin = t * 0.12;
    let angle = uv.x * 2.0 * PI + spin;
    let flow = fract(uv.y - t * 0.08 * (1.0 + params.bass * 0.55));
    let z = (flow - 0.5) * 9.0;
    let base_r = 2.0 - params.bass * 0.28 * k;
    let ripple = sin(angle * 5.0 + z * 1.4 + t * 2.2)
        * 0.10 * (params.mid + params.treble) * k;
    let r = base_r + ripple;

    out.local = vec3<f32>(cos(angle) * r, sin(angle) * r, z)
        * params.object_scale;
    out.uv = vec2<f32>(uv.x, flow);
    // 远端淡出,管子才有纵深。
    let depth_fade = smoothstep(-4.5, 4.5, z);
    out.tint = vec3<f32>(0.4 + depth_fade * 0.7);
    return out;
}

// ── Preset 2:星球(ORBIT)─────────────────────────────────────────────
//
// uv 映成球面:x 绕赤道、y 从南极到北极。低频整体膨胀,高频在表面炸出耀斑,
// 球体自己绕 Y 慢转。
fn place_orbit(uv: vec2<f32>, t: f32, k: f32) -> Placement {
    var out = placement_default(uv);
    let theta = uv.x * 2.0 * PI;
    let phi = (uv.y - 0.5) * PI;
    let flare = snoise(vec3<f32>(theta * 1.5, phi * 1.5, t * 0.7))
        * params.treble * 0.85 * k;
    let r = 2.2 * (1.0 + params.bass * 0.35 * k) + flare;

    let p = vec3<f32>(
        r * cos(phi) * cos(theta),
        r * sin(phi),
        r * cos(phi) * sin(theta));
    let yaw = t * 0.18;
    let cy = cos(yaw);
    let sy = sin(yaw);
    out.local = vec3<f32>(
        p.x * cy - p.z * sy, p.y, p.x * sy + p.z * cy)
        * params.object_scale;
    return out;
}

// ── Preset 3:虚空(VOID)──────────────────────────────────────────────
//
// 无粒子:整层丢到相机背后、透明度归零。留着这一档是为了「只要背景」那个用法。
fn place_void(uv: vec2<f32>) -> Placement {
    var out = placement_default(uv);
    out.local = vec3<f32>(
        (uv.x - 0.5) * 0.01, (uv.y - 0.5) * 0.01, -90.0);
    out.alpha = 0.0;
    out.tint = vec3<f32>(0.0);
    out.cover = 0.0;
    return out;
}

// 唱片档的四个「高分辨率护栏」。原版按 `uCoverRes` 插值,我们的格数固定在默认档
// 之上,于是直接取那一端的定值 —— 格子越密,纹路与描边就得越淡,不然整张糊掉。
const VINYL_EDGE_GUARD: f32 = 0.38;
const VINYL_DEPTH_GUARD: f32 = 0.44;
const VINYL_GROOVE_GUARD: f32 = 0.48;
const VINYL_BEAT_GUARD: f32 = 0.36;

// ── Preset 4:唱片(VINYL)─────────────────────────────────────────────
//
// 一张真唱片的排布:中间是圆形的封面,外面是黑胶纹路,最外一圈白边。整盘自旋,
// 低频与节拍把盘面轻轻撑大。
fn place_vinyl(uv: vec2<f32>, rand: f32, t: f32, k: f32) -> Placement {
    var out = placement_default(uv);
    // 原版的 uBeat 是独立的起拍量,我们没有起拍检测,用低频顶上。
    let beat = params.bass;
    let bass_drive = smoothstep(0.08, 0.78, params.bass + beat * 0.82);
    let high_drive = smoothstep(0.05, 0.46, params.treble);

    let p = (uv - 0.5) * 5.12;
    let spin = t * 0.6;
    let cs = cos(spin);
    let sn = sin(spin);
    let rp = vec2<f32>(p.x * cs - p.y * sn, p.x * sn + p.y * cs);
    let d = length(p);
    let angle0 = atan2(p.y, p.x);
    let record_r = 2.46;
    let cover_r = 1.18;
    let record_alpha = 1.0 - smoothstep(record_r - 0.02, record_r + 0.05, d);
    let cover_mask = 1.0 - smoothstep(cover_r - 0.012, cover_r + 0.018, d);
    let border = exp(-pow((d - cover_r) / 0.064, 2.0)) * VINYL_EDGE_GUARD;
    let outer_rim = exp(-pow((d - (record_r - 0.050)) / 0.055, 2.0))
        * VINYL_EDGE_GUARD;
    let vinyl_n = clamp(
        (d - cover_r) / max(0.001, record_r - cover_r), 0.0, 1.0);
    let swell = 1.0 + bass_drive * 0.012 * VINYL_BEAT_GUARD
        + beat * 0.026 * VINYL_BEAT_GUARD;
    out.alpha = record_alpha;

    if cover_mask > 0.02 {
        // 盘心:封面按圆形裁进来。
        out.uv = p / (cover_r * 2.0) + 0.5;
        let shade = 1.02 + 0.10 * (1.0 - smoothstep(0.0, cover_r, d));
        out.tint = vec3<f32>(shade + border * 0.54);
        out.local = vec3<f32>(
            rp * swell,
            0.040 + border * 0.026 * VINYL_DEPTH_GUARD
                + beat * 0.018 * VINYL_BEAT_GUARD)
            * params.object_scale;
        out.glow = border * 0.30 + bass_drive * 0.075 * VINYL_BEAT_GUARD;
    } else {
        // 盘面:黑胶纹路 + 一圈白边,不采封面。
        let groove = 0.5 + 0.5 * sin((d - cover_r) * 58.0);
        let fine = 0.5 + 0.5 * sin((d - cover_r) * 92.0 + rand * 3.0);
        let tick = smoothstep(0.82, 0.995,
            hash11(floor((angle0 + PI) * 38.0) + floor(d * 72.0) * 2.1));
        let vinyl = vec3<f32>(0.052, 0.054, 0.058)
            + vec3<f32>(0.052 * VINYL_GROOVE_GUARD) * groove
            + vec3<f32>(0.026 * VINYL_GROOVE_GUARD) * fine;
        let white_ring = max(border * 0.92, outer_rim * 0.26);
        out.cover = 0.0;
        out.tint = mix(vinyl, vec3<f32>(0.92, 0.94, 0.94), white_ring)
            + vec3<f32>(tick * high_drive * (0.06 + border * 0.12)
                * VINYL_GROOVE_GUARD);
        out.local = vec3<f32>(
            rp * swell,
            groove * 0.010 * VINYL_GROOVE_GUARD
                + border * 0.024 * VINYL_DEPTH_GUARD
                + bass_drive * vinyl_n * 0.016 * k * VINYL_BEAT_GUARD
                + tick * high_drive * 0.010 * VINYL_GROOVE_GUARD)
            * params.object_scale;
        out.glow = border * 0.32 + outer_rim * 0.12
            + bass_drive * vinyl_n * 0.11 * VINYL_BEAT_GUARD;
    }
    return out;
}

// ── Preset 5:星河(WALLPAPER)─────────────────────────────────────────
//
// 分层的音乐粒子壁纸:前八成的格点排成 5.65 条极光带各自流动,余下两成散成
// 一层带闪烁的纵深尘埃。这一档铺得比视口大得多,是「壁纸」而不是「一张图」。
fn place_wallpaper(uv: vec2<f32>, rand: f32, t: f32) -> Placement {
    var out = placement_default(uv);
    let bass_glow = smoothstep(0.07, 0.78, params.bass) * 0.34;
    let mid_glow = smoothstep(0.07, 0.62, params.mid) * 0.42;
    let high_glow = smoothstep(0.04, 0.46, params.treble) * 0.46;
    let lane = uv.y;
    // 极光把封面色盖掉大半,只留一点底色。
    out.cover = 0.38;

    if lane < 0.80 {
        // 极光带。
        let lane_warp = snoise(
            vec3<f32>(uv.x * 0.42, lane * 1.7, t * 0.026)) * 0.11
            + (hash11(rand * 73.1) - 0.5) * 0.045;
        let warped = clamp(lane + lane_warp, 0.0, 0.80);
        let band_coord = warped / 0.80 * 5.65
            + snoise(vec3<f32>(uv.x * 0.82, lane * 2.25, t * 0.032)) * 0.62;
        let band = floor(band_coord);
        let local = fract(band_coord + hash11(band * 9.13 + rand * 2.4) * 0.18);
        let band_n = clamp((band + 0.5) / 5.65, 0.0, 1.0);
        let seed = hash11(band * 19.17 + rand * 31.0);
        let flow = fract(uv.x
            + t * (0.0034 + band_n * 0.0038 + seed * 0.0022) + seed * 0.53);
        let arc = (flow - 0.5) * PI * (1.35 + band_n * 0.72 + seed * 0.24);
        let arm = sin(arc + band_n * 2.2 + seed * 5.3);
        let radius = 9.2 + band_n * 11.8 + seed * 6.0 + local * 2.9;
        let x = cos(arc * 0.72 + band_n * 0.92 + seed * 1.3) * radius
            + (flow - 0.5) * (13.5 + band_n * 9.5);
        let phase = flow * PI * 2.0 * (0.55 + band_n * 0.24 + seed * 0.10)
            + t * (0.010 + band_n * 0.007) + seed * 5.7;
        let broad = sin(phase) * 0.92;
        let fine = sin(phase * (1.36 + seed * 0.62) - t * 0.044 + seed * 5.0)
            * 0.045;
        let y_base = (band_n - 0.5) * 13.2 + arm * (2.3 + band_n * 1.6)
            + (seed - 0.5) * 1.85
            + snoise(vec3<f32>(band_n * 2.0, flow * 0.62, seed)) * 0.92;
        let ridge_center = 0.43 + (seed - 0.5) * 0.18;
        let ridge = exp(-pow((local - ridge_center) / (0.25 + seed * 0.04), 2.0));
        let soft = smoothstep(0.010, 0.12, lane)
            * (1.0 - smoothstep(0.72, 0.81, lane));
        let noise = snoise(
            vec3<f32>(flow * 1.18 + seed, band_n * 2.0, t * 0.018)) * 0.74;
        let z_layer = mix(-23.5, 15.5, band_n) + (seed - 0.5) * 6.0;
        let pulse = 0.5 + 0.5 * sin(
            phase * (1.7 + seed * 0.9) - t * 0.32 + seed * 6.0);

        out.local = vec3<f32>(
            x + noise * 1.40 + sin(t * 0.012 + seed * 8.0) * 0.22,
            y_base + broad + fine + (local - 0.5) * (0.58 + ridge * 0.14),
            z_layer + broad * 1.35 + noise * 1.85);
        var aurora = mix(
            vec3<f32>(0.52, 0.86, 1.0), vec3<f32>(0.70, 0.58, 1.0), band_n);
        aurora = mix(aurora, vec3<f32>(0.96, 0.98, 0.92), bass_glow * 0.05);
        out.tint = aurora * (0.76 + ridge * 0.86
            + pulse * high_glow * 0.05 + bass_glow * 0.04);
        out.alpha = (0.18 + ridge * 0.78 + pulse * high_glow * 0.035
            + bass_glow * 0.025) * soft;
        out.glow = ridge * (0.12 + mid_glow * 0.05) + pulse * high_glow * 0.045;
    } else {
        // 纵深尘埃。
        let q = (lane - 0.80) / 0.20;
        let seed = hash11(rand * 917.0 + floor(q * 130.0));
        let depth = mix(-32.0, 18.0, seed);
        let drift = fract(uv.x + t * (0.0014 + seed * 0.0048) + seed * 0.63);
        let cluster = snoise(vec3<f32>(seed * 2.0, q * 3.2, t * 0.007));
        let twinkle = pow(
            0.5 + 0.5 * sin(t * (0.24 + seed * 0.42) + rand * 17.0), 5.0);
        let dust = smoothstep(0.22, 0.98, hash11(rand * 661.0 + floor(q * 160.0)));

        out.local = vec3<f32>(
            (drift - 0.5) * (45.0 + seed * 22.0) + cluster * 3.4,
            (hash11(rand * 331.0 + seed * 5.0) - 0.5) * 22.0
                + sin(t * (0.018 + seed * 0.028) + seed * 7.0) * 0.86,
            depth + sin(t * (0.020 + seed * 0.032) + rand * 8.0) * 1.05);
        out.tint = mix(vec3<f32>(1.0), vec3<f32>(0.92, 0.97, 1.0),
            0.62 + twinkle * 0.14)
            * (0.72 + twinkle * 0.62 + bass_glow * 0.025);
        out.alpha = dust * (0.16 + twinkle * 0.46 + high_glow * 0.025
            + bass_glow * 0.018) * (1.0 - q * 0.06);
        out.glow = twinkle * high_glow * 0.055 + dust * bass_glow * 0.030;
    }
    return out;
}

// ── 顶点 ──────────────────────────────────────────────────────────────

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    let t = params.time;
    let base = vertex.position;
    let rand = vertex.tangent.w;
    // 律动强度的真实倍数,同原版:滑块 0.85 → K = 1.36。
    let k = params.intensity * 1.6;
    let s = params.plane_scale;

    var place: Placement;
    switch params.preset {
        case 1u: { place = place_tunnel(vertex.uv, t, k); }
        case 2u: { place = place_orbit(vertex.uv, t, k); }
        case 3u: { place = place_void(vertex.uv); }
        case 4u: { place = place_vinyl(vertex.uv, rand, t, k); }
        case 5u: { place = place_wallpaper(vertex.uv, rand, t); }
        default: { place = place_cover(base, vertex.uv, rand, t, k, s); }
    }
    var local = place.local;

    // 换歌脉冲:整片朝外炸开一点再归位,外加一点纵深上的散开。
    // 幅度按原版 uBurstAmt 那一段的比例,只是这里不掺 loading 形态。
    if params.burst > 0.001 {
        let dir = normalize(local.xy + vec2<f32>(0.0001, 0.0001));
        local = vec3<f32>(
            local.xy + dir * params.burst * 0.42 * s,
            local.z + (rand - 0.5) * params.burst * 0.9 * s);
    }

    // 颜色:有封面就采封面,没有就走原版的默认渐变(紫 → 粉/青)。
    // 换歌时在新旧两张封面之间 mix —— 颜色平滑过渡,不是硬切。
    let sample_uv = clamp(place.uv, vec2(0.0012), vec2(0.9988));
    let new_color = textureSampleLevel(
        cover_texture, cover_sampler, sample_uv, 0.0).rgb;
    let prev_color = textureSampleLevel(
        prev_cover_texture, prev_cover_sampler, sample_uv, 0.0).rgb;
    let cover_color = mix(
        prev_color, new_color, clamp(params.color_mix, 0.0, 1.0));
    let fallback = mix(
        vec3<f32>(0.36, 0.28, 0.72),
        mix(vec3<f32>(0.85, 0.55, 0.95), vec3<f32>(0.45, 0.78, 0.95), place.uv.x),
        place.uv.y);
    let sampled = mix(fallback, cover_color, params.has_cover);
    // `cover` 是「这一颗取多少封面色」:封面档取满,黑胶盘面取零(它的颜色全在
    // tint 里),星河档取一点点当底色。tint 恒是乘子,于是两端都说得通。
    var color = mix(place.tint, sampled * place.tint, place.cover);
    // 节拍提亮,同原版 vBright 的低频/能量项;换歌那一下再亮一档。
    color *= 0.82 + params.bass * 0.10
        + (params.mid + params.treble) * 0.05
        + params.burst * 0.40
        + place.glow;

    // 立方体在**模型空间**展开,不做 billboard。方片时代是反过来的:在视图空间
    // 摊开四个角,粒子才永远正对镜头、永远是正圆。立方体要的恰恰相反 —— 有朝向、
    // 转起来看得见侧面,才叫方块。摊开量仍用世界单位而不是像素:方块与格距同比
    // 投影,窗口怎么变比例都不动。
    let cube_local = local + vertex.corner * params.point_radius;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    let world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local, vec4<f32>(cube_local, 1.0));

    // 面法线跟着实体的变换走(拖动旋转会转整片点云),否则一转起来明暗就跟
    // 方块的实际朝向脱节,看着像贴在表面的花纹而不是立体。
    let world_normal = normalize(
        (world_from_local * vec4<f32>(vertex.tangent.xyz, 0.0)).xyz);
    // 固定方向的一盏「太阳」:顶面最亮、正面居中、底面最暗。不接 bevy 的光照
    // 管线 —— 那要 prepass 与完整的 PBR 绑定,而这里只要六个面分得开。
    let lambert = 0.55 + 0.45 * max(dot(world_normal, CUBE_LIGHT), 0.0);
    color *= lambert;

    var out: VertexOutput;
    out.clip_position = view.clip_from_view * (view.view_from_world * world_position);
    out.color = color;
    out.alpha = place.alpha;
    return out;
}

// ── 片元 ──────────────────────────────────────────────────────────────

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // 方块是实的:没有圆点时代那圈径向衰减了 —— 那圈软边正是「网点」观感的来源,
    // 而像素画要的是硬边色块。透明度只剩预设给的那一层(虚空整层隐身、星河分明暗)。
    let alpha = in.alpha;
    if alpha < params.alpha_cutoff {
        discard;
    }
    return vec4<f32>(clamp(in.color, vec3<f32>(0.0), vec3<f32>(1.6)), alpha);
}
