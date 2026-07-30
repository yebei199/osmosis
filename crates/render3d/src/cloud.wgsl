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
    // 位移幅度的整体倍数:平面比原版放大了多少,起伏就得放大多少。
    motion_scale: f32,
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
    // 涟漪的半径与幅度按平面放大的倍数同比放大,理由同 motion_scale。
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

// 涟漪表的路数,与 cloud.rs 的 RIPPLE_SLOTS 手工对齐。
const RIPPLE_SLOTS: u32 = 12u;

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

// ── 顶点 ──────────────────────────────────────────────────────────────

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    let t = params.time;
    let base = vertex.position;
    let rand = vertex.corner.z;

    // 律动强度的真实倍数,同原版:滑块 0.85 → K = 1.36。再乘上平面放大的补偿,
    // 位移相对整片点云的比例才与原版一致(见 cloud.rs 的 MOTION_SCALE)。
    let K = params.intensity * 1.6 * params.motion_scale;

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

    // 涟漪走 z,与频谱起伏叠加(原版 `pos.z = rippleZ * 1.30 + ...`)。
    let ripple_z = ripple_sum_at(base.xy) * 1.30;
    var local = vec3<f32>(
        base.x, base.y, ripple_z + mid_disp + treble_jitter + bass_breath);

    // 换歌脉冲:整片朝外炸开一点再归位,外加一点纵深上的散开。
    // 幅度按原版 uBurstAmt 那一段的比例,只是这里不掺 loading 形态。
    if params.burst > 0.001 {
        let dir = normalize(vec2<f32>(base.x, base.y) + vec2<f32>(0.0001, 0.0001));
        local = vec3<f32>(
            local.xy + dir * params.burst * 0.42,
            local.z + (rand - 0.5) * params.burst * 0.9);
    }

    // 颜色:有封面就采封面,没有就走原版的默认渐变(紫 → 粉/青)。
    // 换歌时在新旧两张封面之间 mix —— 颜色平滑过渡,不是硬切。
    let sample_uv = clamp(vertex.uv, vec2(0.0012), vec2(0.9988));
    let new_color = textureSampleLevel(
        cover_texture, cover_sampler, sample_uv, 0.0).rgb;
    let prev_color = textureSampleLevel(
        prev_cover_texture, prev_cover_sampler, sample_uv, 0.0).rgb;
    let cover_color = mix(prev_color, new_color, clamp(params.color_mix, 0.0, 1.0));
    let fallback = mix(
        vec3<f32>(0.36, 0.28, 0.72),
        mix(vec3<f32>(0.85, 0.55, 0.95), vec3<f32>(0.45, 0.78, 0.95), vertex.uv.x),
        vertex.uv.y);
    var color = mix(fallback, cover_color, params.has_cover);
    // 节拍提亮,同原版 vBright 的低频/能量项;换歌那一下再亮一档。
    color *= 0.82 + params.bass * 0.10
        + (params.mid + params.treble) * 0.05
        + params.burst * 0.40
        + clamp(abs(ripple_z), 0.0, 1.0) * 0.55;

    // 方片正对相机:位移后的点先进视图空间,再在那里摊开四个角,最后才投影。
    // 在视图空间摊开而不是在模型空间,粒子才永远是正圆而不是随相机变斜的菱形;
    // 摊开量用世界单位而不是像素,点与格距就同比投影,窗口怎么变比例都不动。
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    let world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local, vec4<f32>(local, 1.0));
    var view_position = view.view_from_world * world_position;
    view_position = vec4<f32>(
        view_position.xy + vertex.corner.xy * params.point_radius,
        view_position.zw);

    var out: VertexOutput;
    out.clip_position = view.clip_from_view * view_position;
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
