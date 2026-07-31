// 播放页的反馈 warp 视觉:第一步那个「精心调的单场景」(docs/adr/0010,issue #9)。
//
// 核心机制一条:反馈 + 形变。把上一帧朝中心缩一点、随低频转一点、按 decay 压暗,
// 再叠上这一帧新画的内容 —— 拖影、隧道、生命感全部来自这一步,不需要预设格式。
// 新内容是两圈极坐标可视化:外圈频谱环(半径随各角度的 bin 幅度隆起),
// 内圈波形环(半径随最近的采样摆动)。
//
// 音频从一张 512×2 的纹理进来,照抄 Shadertoy 的约定:v=0.25 行是频谱、
// v=0.75 行是波形(u8,波形静音在 0.5)。CPU 侧(audio::spectrum)已做过
// 包络平滑,这里拿到的谱形是稳的。
//
// 与 navglass.wgsl 同一套独立 pass 骨架:自带全屏三角形,不依赖 bevy。

struct Params {
    // 目标纹理尺寸,物理像素。
    tex_size: vec2<f32>,
    // 播放页时钟,秒。门关着时钟停走,画面与运动一起定格。
    time: f32,
    // 低频包络 0..1,CPU 从频谱行头部均出来 —— 鼓点踩下去整个隧道会"呼吸"。
    bass: f32,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var prev_tex: texture_2d<f32>;
@group(0) @binding(2) var prev_smp: sampler;
@group(0) @binding(3) var audio_tex: texture_2d<f32>;
@group(0) @binding(4) var audio_smp: sampler;

const TAU: f32 = 6.28318530718;

// 全屏三角形,同 navglass.wgsl。
@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

// 余弦调色板(IQ 式),相位取紫/蓝/青一段 —— 与应用 aurora 同调,避免撞出红绿圣诞感。
fn palette(h: f32) -> vec3<f32> {
    let base = 0.5 + 0.5 * cos(TAU * h + vec3<f32>(0.0, 2.1, 4.2));
    return base * vec3<f32>(0.75, 0.62, 1.0);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = frag.xy / u.tex_size;
    let c = vec2<f32>(0.5, 0.5);

    // ── 反馈:上一帧朝中心缩、随低频转、按 decay 压暗 ──
    let ang = 0.0025 + u.bass * 0.02;
    let zoom = 0.995 - u.bass * 0.012;
    let rot = mat2x2<f32>(cos(ang), -sin(ang), sin(ang), cos(ang));
    let p = rot * (uv - c) * zoom + c;
    let fb = textureSample(prev_tex, prev_smp, p).rgb * 0.94;

    // ── 新内容:极坐标里的两圈 ──
    let aspect = u.tex_size.x / max(u.tex_size.y, 1.0);
    let q = (uv - c) * vec2<f32>(aspect, 1.0);
    let r = length(q);
    let a01 = atan2(q.y, q.x) / TAU + 0.5;

    // 频谱环:角度→bin 走对称镜像(低频在右,左右对称),整环随时间缓转。
    let bin = abs(fract(a01 + u.time * 0.01) * 2.0 - 1.0);
    let s = textureSample(audio_tex, audio_smp, vec2<f32>(bin, 0.25)).r;
    let ring_r = 0.30 + s * 0.20 + u.bass * 0.03;
    let ring = 0.006 / (abs(r - ring_r) + 0.004);

    // 波形环:半径直接吃采样摆动,是「现在这一瞬」的形状。
    let w = textureSample(audio_tex, audio_smp, vec2<f32>(fract(a01 + u.time * 0.02), 0.75)).r - 0.5;
    let wave_r = 0.16 + w * 0.12;
    let wave = 0.004 / (abs(r - wave_r) + 0.004);

    let hue = a01 + u.time * 0.03;
    var col = fb;
    col += palette(hue) * ring * (0.25 + s * 0.9);
    col += vec3<f32>(0.024, 0.714, 0.831) * wave * 0.5;

    // 软限幅:反馈会累积能量,不压的话高亮区几帧就烧成纯白。
    col = col / (1.0 + col * 0.10);
    return vec4<f32>(col, 1.0);
}
