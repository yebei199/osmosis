// 播放页封面点云:preset 0(SILK)的顶点位移 + 软边圆点。
//
// 照抄 Mineradio `02-visual/00-pointer-cover-particles.js` 的默认预设(见
// crates/render3d/src/cloud.rs 的模块注释)。三万多颗粒子烘在同一份顶点缓冲里,
// 每颗四个顶点拼一个正对相机的方片;运动学全在这里,CPU 每帧只换 uniform。

#import bevy_pbr::mesh_functions
#import bevy_pbr::mesh_view_bindings::view

struct CloudParams {
    // 播放页时钟,秒。门关即冻结。
    time: f32,
    // 三段电平,各自 0..=1。
    bass: f32,
    mid: f32,
    treble: f32,
    // 律动强度,同原版 uIntensity(默认 0.85)。
    intensity: f32,
    // 封面纹理有没有内容。0 时走默认渐变色,点云在没有封面时不消失。
    has_cover: f32,
    // 片元 alpha 的丢弃门槛。桌面走软边(近 0),安卓走硬边(0.5),
    // 理由见 docs/adr/0012:Adreno 上半透明小元素整片不显示。
    alpha_cutoff: f32,
    // 圆点半径,单位是渲染目标的像素。
    point_size: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: CloudParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var cover_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var cover_sampler: sampler;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    // 格点在点云平面上的基准位。
    @location(0) position: vec3<f32>,
    // (方片角偏移 x, 角偏移 y, 逐粒子随机数)。
    @location(1) corner: vec3<f32>,
    // 封面纹理的采样坐标。
    @location(2) uv: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    // 方片内的局部坐标,画圆点用。
    @location(1) corner: vec2<f32>,
}

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

// ── 顶点 ──────────────────────────────────────────────────────────────

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    let t = params.time;
    let base = vertex.position;
    let rand = vertex.corner.z;

    // 律动强度的真实倍数,同原版:滑块 0.85 → K = 1.36。
    let K = params.intensity * 1.6;

    // SILK:xy 停在格点上,起伏全走 z —— 从正面看是一张随音乐抖动的封面,
    // 侧面看才知道它是有厚度的一层。
    let mid_n = snoise(vec3<f32>(base.x * 1.4, base.y * 1.4, t * 0.55)) * 0.6
        + snoise(vec3<f32>(base.x * 2.8 + 5.0, base.y * 2.8 - 3.0, t * 0.85)) * 0.4;
    let mid_mask = 0.55 + 0.45 * snoise(vec3<f32>(base.x * 0.4, base.y * 0.4, t * 0.18));
    let mid_disp = mid_n * params.mid * 0.55 * mid_mask * K;
    let treble_jitter = snoise(vec3<f32>(
        base.x * 6.5, base.y * 6.5, t * 3.5 + rand * 4.0)) * params.treble * 0.18 * K;
    let bass_breath = snoise(vec3<f32>(
        base.x * 0.35, base.y * 0.35, t * 0.4)) * params.bass * 0.42 * K;

    let local = vec3<f32>(base.x, base.y, mid_disp + treble_jitter + bass_breath);

    // 颜色:有封面就采封面,没有就走原版的默认渐变(紫 → 粉/青)。
    let cover_color = textureSampleLevel(
        cover_texture, cover_sampler, clamp(vertex.uv, vec2(0.0012), vec2(0.9988)), 0.0).rgb;
    let fallback = mix(
        vec3<f32>(0.36, 0.28, 0.72),
        mix(vec3<f32>(0.85, 0.55, 0.95), vec3<f32>(0.45, 0.78, 0.95), vertex.uv.x),
        vertex.uv.y);
    var color = mix(fallback, cover_color, params.has_cover);
    // 节拍提亮,同原版 vBright 的低频/能量项。
    color *= 0.82 + params.bass * 0.10 + (params.mid + params.treble) * 0.05;

    // 方片正对相机:位移后的点先进视图空间,再在那里按像素尺寸摊开四个角。
    // 在视图空间摊开而不是在模型空间,粒子才永远是正圆而不是随相机变斜的菱形。
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    let world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local, vec4<f32>(local, 1.0));
    var clip_position = view.clip_from_view * (view.view_from_world * world_position);
    // NDC 一格 = 视口的一半,故像素尺寸要先除以半个视口再乘 w。
    clip_position = vec4<f32>(
        clip_position.xy + vertex.corner.xy * params.point_size
            / (view.viewport.zw * 0.5) * clip_position.w,
        clip_position.zw);

    var out: VertexOutput;
    out.clip_position = clip_position;
    out.color = color;
    out.corner = vertex.corner.xy;
    return out;
}

// ── 片元 ──────────────────────────────────────────────────────────────

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // 软边圆点:原版是采一张 64×64 的径向渐变贴图,这里直接算出同样的衰减,
    // 省一张纹理和一次采样。
    let dist = length(in.corner);
    let alpha = 0.96 * (1.0 - smoothstep(0.30, 1.0, dist));
    if alpha < params.alpha_cutoff {
        discard;
    }
    return vec4<f32>(clamp(in.color, vec3<f32>(0.0), vec3<f32>(1.6)), alpha);
}
