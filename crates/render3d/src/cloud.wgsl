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
//
// 封面那 96×96 格之外还有一圈铺满视口的方块(cloud/field.rs)。它们的 uv 越出 0..1,
// 这里认的就是这个:颜色取封面边缘外延、糊过的那一份(`haze`),压暗并往视口边淡下去,
// 起伏也弱一档 —— 满屏是方块,主体仍是中间那张清楚的封面。

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
    // 视口在封面 uv 里的四边 (u 左, v 上, u 右, v 下)。封面本身是 0..1。
    bounds: vec4<f32>,
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

// ── 封面外的那一圈 ─────────────────────────────────────────────────────

// 外圈起伏是封面那块的多少。弱一档:外圈是衬底,动得一样大就抢了主体;而且离主轴
// 远的方块往后一退,透视会把它往里拉出一道缝(cloud/field.rs 按这个幅度留了余量)。
// 不是一刀切:离封面边越远越弱,在约 14 格外降到这一档,封面边上两边的起伏是连着的。
const OUTER_MOTION: f32 = 0.35;
// 外圈的亮度:紧贴封面那一格、与视口边上那一格。封面边上明显暗一档,主体的轮廓
// 才立得住;到视口边几乎沉进背景,不在窗口边上切出一道亮边。
const OUTER_DIM_NEAR: f32 = 0.5;
const OUTER_DIM_FAR: f32 = 0.12;
// 淡出距离的下限,uv 单位(约 24 格)。宽版式上下只多出三四格,按实际距离淡的话
// 一出封面就黑了;给个下限,那两条边上的方块只淡一点点。
const MIN_REACH: f32 = 0.25;
// 外圈取第几级 mip:最高一级(1×1)往下数这么多级,即 8×8 那一档 —— 一块方块的颜色
// 是封面边上一大片的平均。越往外再糊一级。
const HAZE_BELOW_TOP: f32 = 3.0;

// 这一格在封面外多远(uv 单位,两轴分开),封面内为 0。
fn beyond_cover(uv: vec2<f32>) -> vec2<f32> {
    return max(abs(uv - vec2<f32>(0.5)) - vec2<f32>(0.5), vec2<f32>(0.0));
}

// 外圈的淡出程度:0 紧贴封面,1 到了视口边。按这一格所在那一侧、封面边到视口边的
// 距离归一,两轴取大的那个(角上那一片随离得更远的那条边)。
fn field_fade(uv: vec2<f32>, beyond: vec2<f32>) -> f32 {
    let reach = vec2<f32>(
        select(-params.bounds.x, params.bounds.z - 1.0, uv.x > 0.5),
        select(-params.bounds.y, params.bounds.w - 1.0, uv.y > 0.5));
    let f = beyond / max(reach, vec2<f32>(MIN_REACH));
    return clamp(max(f.x, f.y), 0.0, 1.0);
}

// 封面边缘外延、糊过的颜色:uv 夹回封面边上,取高几级的 mip。夹回去就是「边缘外延」——
// 封面右边那一列的颜色往右一路铺开;高级 mip 让它是一片色而不是一条条拉丝。
// 没有 mip 的图(占位图只有 1 级)取到的就是第 0 级,不出错。
fn haze(tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, fade: f32) -> vec3<f32> {
    let top = f32(textureNumLevels(tex)) - 1.0;
    let level = clamp(top - HAZE_BELOW_TOP + fade, 0.0, top);
    return textureSampleLevel(tex, samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), level).rgb;
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

// 下标 → [0,1) 的散列,照搬原版的 `hash11`。
fn hash11(p: f32) -> f32 {
    return fract(sin(p * 127.1) * 43758.5453123);
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
    out.local = vec3<f32>(
        base.x, base.y,
        mid_disp + treble_jitter + bass_breath);
    // 曾经这里还叠一路「指针涟漪」:鼠标所到之处顶起一个高斯包。它照抄自参照
    // 项目,而在本仓库里**从未真正生效过** —— uniform 的字段顺序两边写反了,
    // 着色器一直从填充字节里读它的幅度,读到的是 0。顺序对齐之后它第一次露面,
    // 是一个跟着鼠标走的巨大空洞,遂整条删除(见 docs/adr/0014 同一类判断)。
    out.glow = 0.0;
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

    // 封面外那一圈:离封面多远、淡到哪儿、起伏弱多少。封面内三者都是「不变」。
    let beyond = beyond_cover(vertex.uv);
    let outer = max(beyond.x, beyond.y) > 0.0;
    let fade = field_fade(vertex.uv, beyond);
    let motion = mix(1.0, OUTER_MOTION, smoothstep(0.0, 0.15, length(beyond)));

    // 只剩「封面」一档,所以这里没有分支。子预设那条 seam 已随 docs/adr/0041 收掉:
    // 播放页的顶层选择是「效果」(封面实物 / 点云),点云自己不再分档。
    let place = place_cover(base, vertex.uv, rand, t, k * motion, s);
    var local = place.local;

    // 换歌脉冲:整片朝外炸开一点再归位,外加一点纵深上的散开。
    // 幅度按原版 uBurstAmt 那一段的比例,只是这里不掺 loading 形态。
    if params.burst > 0.001 {
        let dir = normalize(local.xy + vec2<f32>(0.0001, 0.0001));
        local = vec3<f32>(
            local.xy + dir * params.burst * 0.42 * s,
            local.z + (rand - 0.5) * params.burst * 0.9 * s);
    }

    // §1 换歌过渡的另外两段(docs/design/handoff-shaders.md):全局进度
    // 就是 color_mix(start 归 0,900ms 计满),不另加 uniform。
    // 错峰:每颗按到封面中心的距离推迟自己的相位 —— 中心先落、边缘后落。
    // 封面那块的距离在 0..1(角上是 1),外圈接着往外涨,封顶 1.7:外圈比封面的角
    // 更晚落,但最远那一格也还留四成的进度走完自己那一段。
    let tr = clamp(params.color_mix, 0.0, 1.0);
    let center_d = min(
        length(place.uv - vec2<f32>(0.5)) * 1.4142, 1.7);
    let phase_delay = center_d * 0.35;
    let tp = clamp(
        (tr - phase_delay) / max(1.0 - phase_delay, 0.001),
        0.0, 1.0);
    if tr < 1.0 {
        // 中段脱格:伪 curl 流场(三对 snoise 差分),包络两端为 0 ——
        // 起点与落点都在网格上,「散开」只存在于路上。过渡结束整段跳过,
        // 平帧一次 snoise 都不多算。
        let flow = sin(clamp((tp - 0.08) / 0.84, 0.0, 1.0) * 3.14159265);
        let np = vec3<f32>(place.uv * 3.0, tr * 1.6);
        let curl = vec3<f32>(
            snoise(np + vec3<f32>(0.0, 13.7, 5.2)) - snoise(np + vec3<f32>(7.3, 0.0, 9.1)),
            snoise(np + vec3<f32>(3.1, 8.8, 0.0)) - snoise(np + vec3<f32>(0.0, 4.4, 11.6)),
            (snoise(np + vec3<f32>(9.9, 2.2, 6.6)) - snoise(np + vec3<f32>(5.5, 12.1, 0.0))) * 0.6);
        local += curl * flow * 0.5 * s;
    }

    // 颜色:有封面就采封面,没有就走原版的默认渐变(紫 → 粉/青)。
    // 换歌时在新旧两张封面之间按**这一颗自己的**错峰相位 mix ——
    // 先落位的先换色,颜色跟着形体走,不是全场同一刻硬切。
    // 外圈取糊过的那一份(`haze`),封面那块照旧取第 0 级。两张都采只在换歌渐变期间:
    // 平帧上「上一首」那张整个用不着,外圈那一两万次采样不该白付一半。
    let sample_uv = clamp(place.uv, vec2(0.0012), vec2(0.9988));
    var new_color: vec3<f32>;
    var prev_color: vec3<f32>;
    if outer {
        new_color = haze(cover_texture, cover_sampler, place.uv, fade);
        prev_color = new_color;
        if tp < 1.0 {
            prev_color = haze(prev_cover_texture, prev_cover_sampler, place.uv, fade);
        }
    } else {
        new_color = textureSampleLevel(
            cover_texture, cover_sampler, sample_uv, 0.0).rgb;
        prev_color = textureSampleLevel(
            prev_cover_texture, prev_cover_sampler, sample_uv, 0.0).rgb;
    }
    let cover_color = mix(prev_color, new_color, tp);
    // 默认渐变按夹回封面里的 uv 取:外圈越出 0..1 的 uv 直接代进 mix 会外推出负的颜色。
    let gradient_uv = clamp(place.uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let fallback = mix(
        vec3<f32>(0.36, 0.28, 0.72),
        mix(vec3<f32>(0.85, 0.55, 0.95), vec3<f32>(0.45, 0.78, 0.95), gradient_uv.x),
        gradient_uv.y);
    let sampled = mix(fallback, cover_color, params.has_cover);
    // `cover` 是「这一颗取多少封面色」:封面档取满,黑胶盘面取零(它的颜色全在
    // tint 里),星河档取一点点当底色。tint 恒是乘子,于是两端都说得通。
    var color = mix(place.tint, sampled * place.tint, place.cover);
    // 节拍提亮,同原版 vBright 的低频/能量项;换歌那一下再亮一档。
    color *= 0.82 + params.bass * 0.10
        + (params.mid + params.treble) * 0.05
        + params.burst * 0.40
        + place.glow;
    // 外圈压暗,往视口边沉下去。封面边上那一步是有意的:主体的轮廓要立得住。
    if outer {
        color *= mix(OUTER_DIM_NEAR, OUTER_DIM_FAR, smoothstep(0.0, 1.0, fade));
    }

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
