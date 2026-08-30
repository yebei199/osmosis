// 卡墙上「正在放的那一张」的全息闪卡效果。
//
// 只有一张卡走这条材质(见 crates/ui/src/wall_drive.rs 的 foil_slot):闪光
// 是「在播」的视觉信号,不是装饰。光泽常驻缓慢流动 —— 实体闪卡的彩虹是随
// 视角变的,屏幕上没有视角可用,只能拿时间当替代驱动源。
//
// 底图是 ui 侧烘好的卡面:圆角、描边、四周一圈投影全在 alpha 里。闪光按
// alpha 收在卡面内,不许爬到投影那一圈上 —— 会闪的影子不像闪卡,像发光的雾。

#import bevy_pbr::forward_io::VertexOutput

struct FoilParams {
    // 秒。由 render3d 侧按帧计数推,与播放页时钟无关。
    time: f32,
    // 随深度压暗的系数,与普通卡的 base_color 同义。
    dim: f32,
    _pad: vec2<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> params: FoilParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var cover_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var cover_sampler: sampler;

// 余弦调色板(Inigo Quilez 那一套):一个标量绕出一圈彩虹,
// 比查表短,比 hsv2rgb 少一堆分支。
fn palette(t: f32) -> vec3<f32> {
    return 0.5 + 0.5 * cos(6.28318 * (vec3<f32>(1.0, 1.0, 1.0) * t
        + vec3<f32>(0.0, 0.33, 0.67)));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(cover_texture, cover_sampler, in.uv);
    let uv = in.uv;

    // 斜向的干涉条纹:两组不同频率、反向流动的波叠加,叠出来的拍频
    // 才不像一把匀速走过的梳子。
    let diag = uv.x + uv.y;
    let bands = sin(diag * 11.0 - params.time * 0.55)
        * sin((uv.x - uv.y) * 7.0 + params.time * 0.31);
    let holo = palette(diag * 0.5 + bands * 0.12 + params.time * 0.05);

    // 一道更亮的高光带斜着扫过,周期比条纹长得多:整片彩虹是底,
    // 这一道才是「翻动卡片」的那一下。
    let sweep_pos = fract(params.time * 0.13);
    let sweep = exp(-pow((fract(diag * 0.5 - sweep_pos + 0.5) - 0.5) * 6.5, 2.0));

    // 闪光只落在卡面上:投影那一圈 alpha 低,由它自己滤掉。
    let face = smoothstep(0.55, 0.95, base.a);
    let shine = face * (0.28 + 0.55 * sweep);

    let lit = base.rgb * params.dim;
    // 加光要看这一点还剩多少余量。纯加性的话浅色封面会被推到过曝,
    // 线条和字全糊成一片白 —— 封面才是内容,闪卡是它上面的一层膜。
    // 亮处只做**色偏**(零均值,不动亮度),暗处才补整段彩虹。
    let lum = dot(lit, vec3<f32>(0.299, 0.587, 0.114));
    let room = 1.0 - lum;
    let tint = (holo - vec3<f32>(0.5)) * shine * 0.45;
    let glow = holo * shine * room * 0.9;
    let rgb = lit + tint + glow
        + vec3<f32>(sweep * face * 0.16 * room);
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), base.a);
}
