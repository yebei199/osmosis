// 封面实物:封套、它的倒影、承它的地面,一份着色器按 `kind` 分三种画法。
//
// 顶点走 bevy 默认的网格着色器(位置、法线、uv),这里只有片元。光是写死的一盏
// 柔和主光加半球环境光,不接 bevy 的光照管线 —— 那要阴影、prepass 与一整套 PBR 绑定,
// 而这里要的只是「一个被一盏灯照着的纸板」。随音乐动的只有倒角那一圈边缘光。
//
// 半透明分平台(见 crates/render3d/src/sleeve.rs):`dither` 为 1 时用 4×4 有序抖动
// 代替混合 —— Adreno 上 Blend 整片不显示(docs/adr/0012)。

#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view

struct SleeveParams {
    // 地面平面(世界):xyz 朝上的法线,w 偏移。倒影只画在它下面。
    plane: vec4<f32>,
    // 地面 uv 里的脚印:(中心 u, 中心 v, 每台面单位合多少 u, 合多少 v)。
    footprint: vec4<f32>,
    // 封套正面这一帧在世界里的朝向(xyz)。
    facing: vec4<f32>,
    // 封套相对台面的 (yaw, pitch),弧度。光泽跟着它滑。
    tilt: vec2<f32>,
    // 0 封套、1 倒影、2 地面。
    kind: u32,
    // 1 = 有序抖动代替混合(安卓)。
    dither: u32,
    // 封面显影程度,0 是空封套。
    cover: f32,
    // 平滑过的低频,0..1。
    glow: f32,
    // 一个台面单位合多少世界单位。
    scale: f32,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: SleeveParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var cover_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var cover_sampler: sampler;

// 纸边与背面:近黑、带一点绿的色相,与沉浸层的底同族,不抢封面的颜色。
const EDGE: vec3<f32> = vec3<f32>(0.030, 0.036, 0.033);
// 空封套的正面:比纸边亮一档的哑光灰,上亮下暗一点,读得出是一张「还没印」的封套。
const BLANK_TOP: vec3<f32> = vec3<f32>(0.090, 0.100, 0.095);
const BLANK_BOTTOM: vec3<f32> = vec3<f32>(0.040, 0.046, 0.043);
// 地面的颜色,即沉浸层底色 #060c09 的线性值。
const FLOOR_TINT: vec3<f32> = vec3<f32>(0.0018, 0.0037, 0.0027);
// 边缘光的颜色:略暖的白。
const RIM_TINT: vec3<f32> = vec3<f32>(1.0, 0.97, 0.92);

// 4×4 Bayer 阈值,0..1。安卓上半透明靠它挑像素。
fn bayer4(frag: vec2<f32>) -> f32 {
    var m = array<f32, 16>(
        0.0, 8.0, 2.0, 10.0,
        12.0, 4.0, 14.0, 6.0,
        3.0, 11.0, 1.0, 9.0,
        15.0, 7.0, 13.0, 5.0);
    let x = u32(frag.x) % 4u;
    let y = u32(frag.y) % 4u;
    return (m[y * 4u + x] + 0.5) / 16.0;
}

// 半透明的出口:桌面直接交 alpha 给混合;安卓按抖动阈值丢像素,留下的不透明。
fn translucent(color: vec3<f32>, alpha: f32, frag: vec2<f32>) -> vec4<f32> {
    if params.dither == 1u {
        if alpha <= bayer4(frag) {
            discard;
        }
        return vec4<f32>(color, 1.0);
    }
    return vec4<f32>(color, clamp(alpha, 0.0, 1.0));
}

// 地面:脚印周围一汪暗面往外淡开,封套正下方再压一道窄而实的接触阴影。
// 倒影不在这里画 —— 它是一份镜像网格,按排序画在地面之上(见 mesh.rs 的 FLOOR_FAR)。
fn floor_color(in: VertexOutput) -> vec4<f32> {
    // 换回台面单位:x 横向,y 纵深(朝相机为正)。
    let d = (in.uv - params.footprint.xy) / params.footprint.zw;
    let pool = 1.0 - smoothstep(0.15, 1.0, length(d / vec2<f32>(1.45, 0.85)));
    let contact = 1.0 - smoothstep(0.0, 1.0, length(d / vec2<f32>(0.56, 0.09)));
    let alpha = 1.0 - (1.0 - 0.55 * pool) * (1.0 - 0.6 * contact);
    return translucent(FLOOR_TINT, alpha, in.position.xy);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // 采样放在一切分支之前,并且取固定 mip:后面有按像素的 discard,
    // 隐式求导的 textureSample 在非一致控制流里不合法。封面没有 mip,取 0 级不丢东西。
    let uv = clamp(in.uv, vec2<f32>(0.0), vec2<f32>(1.0));
    let printed = textureSampleLevel(cover_texture, cover_sampler, uv, 0.0).rgb;

    if params.kind == 2u {
        return floor_color(in);
    }

    // 倒影只活在地面以下。拖动把封套的一角转到地面以下时,它的镜像会翻到地面以上 ——
    // 那一截剪掉,不然地面上方会凭空多出一块倒着的封面。
    let height = dot(in.world_position.xyz, params.plane.xyz) - params.plane.w;
    if params.kind == 1u && height > 0.0 {
        discard;
    }

    let n = normalize(in.world_normal);
    let v = normalize(view.world_position - in.world_position.xyz);

    // 正面印封面,倒角往外转、背面是纸边。按法线与正面朝向的夹角过渡,
    // 倒角靠正面的那半圈因此也印着封面(真封套的印刷是包边的)。
    let front = smoothstep(0.55, 0.95, dot(n, params.facing.xyz));
    let blank = mix(BLANK_TOP, BLANK_BOTTOM, uv.y);
    let face = mix(blank, printed, clamp(params.cover, 0.0, 1.0));
    let albedo = mix(EDGE, face, front);

    // 一盏柔和的主光从左上前方来,加一层上亮下暗的半球环境光。系数调到正面在
    // 静止时约等于 1:封面不该比它本来的颜色更暗或更亮。
    let key = normalize(vec3<f32>(-0.38, 0.52, 0.76));
    let light = 0.74 + 0.10 * n.y + 0.40 * max(dot(n, key), 0.0);

    // 一道很淡的斜向光泽,位置随封套的偏转滑过正面 —— 拿在手里转一下,
    // 光在纸面上走一段。只在正面,亮处减半,不把浅色封面推到过曝。
    let diag = (uv.x + uv.y) * 0.5;
    let center = 0.28 + params.tilt.x * 2.6 - params.tilt.y * 1.8;
    let off = (diag - center) / 0.11;
    let lum = dot(face, vec3<f32>(0.2126, 0.7152, 0.0722));
    let sheen = exp(-off * off) * 0.06 * (1.0 - 0.6 * lum) * front;

    // 边缘光:掠射处(法线与视线近乎垂直)最亮,只落在倒角与纸边上。
    // 低频把它推亮 —— 这是封面实物唯一跟着音乐动的东西。
    let ndv = clamp(dot(n, v), 0.0, 1.0);
    let grazing = 1.0 - ndv;
    let rim = grazing * grazing * (1.0 - front);
    let rim_light = RIM_TINT * rim * (0.10 + 0.70 * clamp(params.glow, 0.0, 1.0));

    let color = albedo * light + vec3<f32>(sheen) + rim_light;

    if params.kind == 1u {
        // 倒影:压暗一些,离地面越远越淡,在约半个封套高处淡尽。
        let depth = max(-height, 0.0) / max(params.scale, 0.0001);
        let strength = 0.30 * (1.0 - smoothstep(0.0, 0.42, depth));
        return translucent(color * 0.85, strength, in.position.xy);
    }
    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
