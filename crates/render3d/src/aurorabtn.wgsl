// 光带按钮:六个变体共用的片元程序(docs/design/handoff-shaders.md §9/§10)。
// 数学逐行移植自 docs/design/aurora-button.js 的 GLSL 参考实现,只做语法换壳;
// fbm 四阶(按钮尺寸下与六阶无肉眼差别)。配色一律由 uniform 里的四色板给,
// 全光谱色相环只在 mode = 0(仅 Home 空槽,docs/design.md 铁律)。
//
// 与 navglass 同构的独立 wgpu pass:全屏三角形,片元里按 origin/res 折回
// 按钮局部坐标;圆角胶囊由 SDF 裁形,输出预乘 alpha。

struct Params {
    // 本颗按钮在目标纹理里的原点与尺寸,物理像素。
    origin: vec2<f32>,
    res: vec2<f32>,
    time: f32,
    seed: f32,
    speed: f32,
    // 静息 0.12 → 悬停 1.0。
    amp: f32,
    // 0 = 全光谱,1 = 绿色板(只影响 ribbon 的色相)。
    mode: f32,
    radius: f32,
    // ribbon 的光带条数 1..4。
    bands: f32,
    // 0 ribbon 1 nebula 2 fluid 3 glass 4 progress 5 prism。
    variant: f32,
    progress: f32,
    // 棱柱转到哪儿了,单位是「面」:0 面 A、1 面 B、2 面 C,面间为小数。
    flip: f32,
    // 按钮内归一化指针位置。
    pointer: vec2<f32>,
    // 四色板:底 / 主 / 次 / 高光(w 分量空置)。
    col_a: vec4<f32>,
    col_b: vec4<f32>,
    col_c: vec4<f32>,
    col_d: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Params;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vi << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(vi & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn sd_round(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - r;
}

fn hsv(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = fract(vec3<f32>(h) + vec3<f32>(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0;
    return v * mix(vec3<f32>(1.0), clamp(abs(k) - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

fn hash21(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 456.21));
    p += dot(p, p + 45.32 + u.seed);
    return fract(p.x * p.y);
}

fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    var f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), f.x),
        mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), f.x),
        f.y);
}

// 四阶 fbm。
fn fbm(p_in: vec2<f32>) -> f32 {
    var v = 0.0;
    var a = 0.53;
    var p = p_in;
    let rot = mat2x2<f32>(0.80, -0.60, 0.60, 0.80);
    for (var i = 0; i < 4; i++) {
        v += a * vnoise(p);
        p = rot * p * 2.02 + vec2<f32>(17.13, 9.27);
        a *= 0.49;
    }
    return v;
}

fn soft_blob(p: vec2<f32>, c: vec2<f32>, r: f32, soft: f32) -> f32 {
    return 1.0 - smoothstep(r - soft, r + soft, length(p - c));
}

// ── 变体 0:光带 ────────────────────────────────────────────

fn center_at(x: f32, t: f32, s: f32, sp: f32) -> f32 {
    return 0.50
        + 0.150 * sin(6.2831 * (x * 0.80) + t * 0.60 * sp + s)
        + 0.085 * sin(6.2831 * (x * 1.70) - t * 0.45 * sp + s * 1.7)
        + 0.045 * sin(6.2831 * (x * 3.10) + t * 0.90 * sp + s * 0.6);
}

fn width_at(x: f32, t: f32, s: f32, sp: f32) -> f32 {
    return (0.052 + 0.030 * sin(6.2831 * (x * 1.30) - t * 0.50 * sp + s))
        * mix(1.0, 0.58, clamp((u.bands - 1.0) / 3.0, 0.0, 1.0));
}

fn band_at(y: f32, c: f32, w: f32, dy: f32) -> f32 {
    let d = (y - c - dy) / w;
    return exp(-d * d * 2.2);
}

fn render_ribbon(p: vec2<f32>, t: f32) -> vec3<f32> {
    var col = vec3<f32>(0.0);

    for (var i = 0; i < 4; i++) {
        let fi = f32(i);
        let on = step(fi + 0.5, u.bands);
        let sd2 = u.seed + fi * 2.37;
        let sp = 1.0 + fi * 0.16;
        let bw = 1.0 - fi * 0.15;

        // 斜率越大色散越强 —— 只有拐弯处出彩虹边,直段是白的。
        let h = 0.004;
        let slope = abs(center_at(p.x + h, t, sd2, sp) - center_at(p.x - h, t, sd2, sp)) / (2.0 * h);
        let disp = (0.006 + 0.030 * clamp(slope * 0.16, 0.0, 1.0)) * mix(0.55, 1.0, u.amp);

        // 方向性动态模糊:沿流动方向取 6 个样本。
        var acc = vec3<f32>(0.0);
        var core = 0.0;
        for (var k = 0; k < 6; k++) {
            let fk = f32(k);
            let wgt = 1.0 - fk / 7.0;
            let xo = p.x - fk * 0.0060;
            let c0 = center_at(xo, t, sd2, sp);
            let w0 = width_at(xo, t, sd2, sp);
            acc += vec3<f32>(
                band_at(p.y, c0, w0, -disp),
                band_at(p.y, c0, w0, 0.0),
                band_at(p.y, c0, w0, disp)) * wgt;
            core += band_at(p.y, c0, w0, 0.0) * wgt;
        }
        acc /= 3.9;
        core /= 3.9;

        let glow = exp(-pow((p.y - center_at(p.x, t, sd2, sp)) / (width_at(p.x, t, sd2, sp) * 3.4), 2.0)) * 0.26;

        var tint: vec3<f32>;
        if (u.mode < 0.5) {
            // 全光谱:色相沿 x 与时间推移。只准出现在 Home 空槽(docs/design.md)。
            tint = hsv(fract(p.x * 0.85 + t * 0.07 + core * 0.22 + fi * 0.21), 0.85, 1.0);
        } else {
            // 收进绿色板:主绿 → 青 → 黄绿,色散边混一点紫。
            let hh = 0.30 + 0.13 * sin(6.2831 * (p.x * 0.7) + t * 0.25 + fi * 1.10) + core * 0.05 + fi * 0.035;
            tint = hsv(hh, 0.68, 1.0);
            tint = mix(tint, vec3<f32>(0.62, 0.56, 0.92), clamp(slope * 0.05, 0.0, 0.35) * 0.6);
        }

        var c = acc * tint * 1.75;
        c += vec3<f32>(glow) * tint * 0.7;
        c += vec3<f32>(pow(core, 3.0)) * 1.9; // 白热核心
        col += c * bw * on;
    }

    // 参考实现连除了两次(bug 兼容:观感以设计稿为准,照抄)。
    col /= (0.62 + u.bands * 0.30);
    col *= mix(0.16, 1.0, u.amp);
    col /= (0.62 + u.bands * 0.30);
    return col;
}

// ── 变体 1:星云 ────────────────────────────────────────────

fn render_nebula(uv: vec2<f32>, p_in: vec2<f32>, t: f32) -> vec3<f32> {
    let ptr = (u.pointer - 0.5) * vec2<f32>(u.res.x / max(u.res.y, 1.0), 1.0);
    let delta = p_in - ptr;
    let d_to_p = length(delta);
    let infl = exp(-d_to_p * 4.6) * u.amp;
    let ang = infl * 1.7;
    let swirl = mat2x2<f32>(cos(ang), -sin(ang), sin(ang), cos(ang));
    let p = ptr + swirl * delta + normalize(delta + vec2<f32>(0.0001)) * infl * 0.08;

    let drift = vec2<f32>(t * 0.22, -t * 0.13);
    let q = vec2<f32>(
        fbm(p * 1.35 + drift + u.seed),
        fbm(p * 1.35 + vec2<f32>(5.2, 1.3) - drift * 0.85));
    let r = vec2<f32>(
        fbm(p * 2.0 + 3.6 * q + t * 0.10),
        fbm(p * 2.0 + 3.0 * q - t * 0.085));
    let cloud = fbm(p * 1.7 + 4.2 * r);
    let veins = fbm(p * 4.0 - 2.0 * q + t * 0.065);
    let n = smoothstep(0.18, 0.91, cloud * 0.9 + veins * 0.22);

    var col = mix(
        mix(u.col_a.rgb, u.col_b.rgb, smoothstep(0.06, 0.62, n)),
        mix(u.col_b.rgb, u.col_c.rgb, smoothstep(0.30, 0.82, n)),
        smoothstep(0.26, 0.72, n));
    col = mix(col, u.col_d.rgb, smoothstep(0.80, 0.98, n));
    col += u.col_d.rgb * pow(max(cloud - 0.63, 0.0), 2.0) * 1.05;
    col *= 0.78 + 0.34 * smoothstep(0.15, 0.9, veins);

    // 星点:格子里挑约百分之一的格子点亮,再各自闪烁。
    let cell = fract(uv * vec2<f32>(96.0, 42.0)) - 0.5;
    let rnd = hash21(floor(uv * vec2<f32>(96.0, 42.0)));
    let star = step(0.988, rnd) * smoothstep(0.085, 0.0, length(cell));
    col += star * (0.35 + 0.65 * (0.5 + 0.5 * sin(t * (1.0 + rnd * 2.4) + rnd * 40.0))) * mix(u.col_c.rgb, u.col_d.rgb, rnd);
    col += u.col_d.rgb * exp(-d_to_p * 7.0) * u.amp * 0.28;
    return col;
}

// ── 变体 2:流体玻璃(reveal 左侧开闸,文字区不被糊住)────────

fn render_fluid(uv: vec2<f32>, p_in: vec2<f32>, t: f32, dens: ptr<function, f32>) -> vec3<f32> {
    let ptr_p = (u.pointer - 0.5) * vec2<f32>(u.res.x / max(u.res.y, 1.0), 1.0);
    let dl = p_in - ptr_p;
    let d = length(dl);
    let field = exp(-d * d * 7.2) * u.amp;
    let nrm = dl / max(d, 0.035);
    let p = p_in + nrm * field * 0.115;

    let sv = vec2<f32>(u.seed * 1.713, u.seed * 0.937);
    let w1 = fbm(p * 1.22 + sv + vec2<f32>(t * 0.075, -t * 0.052));
    let w2 = fbm(p * 1.54 - sv * 0.37 + vec2<f32>(-t * 0.057, t * 0.064) + w1 * 0.82);
    let q = p + (vec2<f32>(w1, w2) - 0.5) * 0.58;
    let broad = fbm(q * 1.12 + vec2<f32>(t * 0.041, -t * 0.033));
    let detail = fbm(q * 2.18 + vec2<f32>(-t * 0.083, t * 0.057) + broad * 0.95);
    let ribbon_n = 0.5 + 0.5 * sin(q.x * 3.15 + q.y * 0.76 + detail * 5.0 + t * 0.25 + u.seed);

    var fluid = mix(u.col_b.rgb, u.col_c.rgb, smoothstep(0.16, 0.88, broad * 0.61 + ribbon_n * 0.39));
    fluid = mix(
        fluid,
        u.col_a.rgb,
        smoothstep(0.43, 0.84, detail * 0.69 + (0.5 + 0.5 * sin(q.y * 4.2 - q.x * 0.8 - t * 0.17)) * 0.31) * 0.74);

    let aspect = u.res.x / max(u.res.y, 1.0);
    let haze = clamp(
        soft_blob(p, vec2<f32>(aspect * 0.23 + 0.12 * sin(t * 0.08 + u.seed), 0.16 * cos(t * 0.11 + u.seed)), 0.52, 0.38) * 0.72
            + soft_blob(p, vec2<f32>(aspect * 0.39 + 0.10 * cos(t * 0.07 - u.seed), -0.24 + 0.11 * sin(t * 0.09)), 0.43, 0.34) * 0.58,
        0.0, 1.0);
    // reveal:左侧接近 0,右侧才让流体出现 —— WGSL 版必须保留(§10)。
    let reveal = clamp(
        smoothstep(0.055, 0.735, uv.x + (0.5 - broad) * 0.27 + 0.070 * sin(uv.y * 4.0 + t * 0.12)) * mix(0.70, 1.0, haze),
        0.0, 1.0);

    let spec = pow(clamp(1.0 - abs(detail - 0.52) * 2.0, 0.0, 1.0), 5.0) * reveal;
    let caustic = pow(clamp(0.52 + 0.48 * sin((q.x - q.y) * 5.2 + detail * 7.0 - t * 0.18), 0.0, 1.0), 7.0) * reveal;
    var col = mix(fluid, u.col_d.rgb, spec * 0.20 + caustic * 0.10);
    col *= 0.78 + 0.25 * haze;
    let filament = smoothstep(0.48, 0.86, detail) * reveal;
    *dens = clamp(reveal * (0.36 + 0.48 * haze) + filament * 0.22 + field * 0.28, 0.0, 1.0);
    return col;
}

// ── 变体 3:液态玻璃 ────────────────────────────────────────

fn render_glass(uv: vec2<f32>, p_in: vec2<f32>, t: f32, dens: ptr<function, f32>) -> vec3<f32> {
    let sv = vec2<f32>(u.seed * 1.2, u.seed * 0.7);
    let w = fbm(p_in * 1.9 + sv + vec2<f32>(t * 0.05, -t * 0.04));
    let q = p_in + (vec2<f32>(w, fbm(p_in * 2.2 - sv + t * 0.03)) - 0.5) * 0.32;
    let detail = fbm(q * 2.6 + t * 0.05);
    let caustic = pow(clamp(0.52 + 0.48 * sin((q.x - q.y) * 6.4 + detail * 8.0 - t * 0.22), 0.0, 1.0), 8.0);
    let spec = pow(clamp(1.0 - abs(detail - 0.5) * 2.2, 0.0, 1.0), 6.0);
    let sheen = smoothstep(0.35, 0.0, abs(uv.y - (0.30 + 0.10 * sin(t * 0.4)))) * 0.35;
    let col = mix(u.col_a.rgb, u.col_b.rgb, detail * 0.5)
        + u.col_d.rgb * (spec * 0.55 + caustic * 0.40)
        + u.col_c.rgb * sheen * 0.5;
    *dens = clamp(0.14 + spec * 0.5 + caustic * 0.45 + sheen * 0.4, 0.0, 1.0);
    return col;
}

// 三棱柱条身(#83)。柱轴沿条身长边(x),绕轴转;每转 120° 换一面。
//
// 真透视而不是压扁:相机架在 z = CAM_Z 处,朝 -z 看,只对 y/z 做透视除法
// (x 方向保持正交 —— 条身很宽,给 x 也加透视会把两端拉成鱼眼)。
// 逐像素朝三个面的平面投射线,取最近的正对面,命中点在面内的坐标就是贴图的 v。
//
// 这一层只是**装饰**:键仍是条上那层 2D 元素,命中测试归 slint,
// 转面时按钮不跟着歪(docs/adr/0010、0028)。
const CAM_Z: f32 = 3.0;
// 正三角形外接圆半径取 1,内切圆(面到轴的距离)因此是 0.5。
const APOTHEM: f32 = 0.5;
const TAU_3: f32 = 2.0943951;   // 120°

fn render_prism(uv: vec2<f32>, t: f32, dens: ptr<function, f32>) -> vec3<f32> {
    // 屏幕纵向映到相机像平面。张角是这里唯一的调节旋钮:条身宽是高的九倍,
    // 张角小了三个面挤成一条缝、根本看不出在转,大了侧面又会吃掉整条。
    // 1.8 是实拍挑出来的:侧面各占约两成高,棱边看得见,中间那面还留得住文字。
    let sy = (uv.y - 0.5) * 1.8;
    let ro = vec3<f32>(0.0, 0.0, CAM_Z);
    let rd = normalize(vec3<f32>(0.0, sy, -1.0));

    let ang = u.flip * TAU_3;
    var best_dist = 1e9;
    var best_v = 0.0;
    var best_face = 0.0;
    var best_facing = 0.0;

    for (var i = 0; i < 3; i = i + 1) {
        // 第 i 面的外法线,绕 x 轴转 ang 之后的朝向。
        let a = ang + f32(i) * TAU_3;
        let n = vec3<f32>(0.0, cos(a), sin(a));
        let facing = -dot(rd, n);
        // 背对相机的面不画:那是柱体的另一侧。
        if (facing <= 0.001) { continue; }
        let dist = (APOTHEM - dot(ro, n)) / dot(rd, n);
        if (dist <= 0.0 || dist >= best_dist) { continue; }
        // 命中点在该面内的横坐标(沿柱面切向),用作贴图的 v。
        let hit = ro + rd * dist;
        let tangent = vec3<f32>(0.0, -sin(a), cos(a));
        best_dist = dist;
        best_v = dot(hit, tangent);
        best_face = f32(i);
        best_facing = facing;
    }

    // 一个面也没命中(理论上不会,棱柱是闭合的)——退回底色。
    if (best_dist > 1e8) {
        *dens = 0.0;
        return u.col_a.rgb;
    }

    // 每面各取一段贴图,面与面之间不重样 —— 三面共用一段的话,
    // 转过去和没转看着一模一样。
    //
    // 错位只动 **v**,绝不动 u:render_fluid 的 reveal 左闸是按 uv.x 开的,
    // 那道闸保的正是条身左边的曲名与时间。动了 u 就等于把闸挪走,
    // 面 B、面 C 上流体会漫过文字区,字直接读不出来(实拍踩过)。
    var fl = 1.0;
    let face_uv = vec2<f32>(uv.x, fract(best_v * 0.5 + 0.5 + best_face * 0.31));
    let fp = (face_uv - 0.5) * vec2<f32>(u.res.x / max(u.res.y, 1.0), 1.0);
    let base = render_fluid(face_uv, fp, t, &fl);

    // 正对相机的面最亮,偏过去的面按余弦压暗 —— 那道明暗就是「它是个柱体」
    // 唯一说得出口的证据(条身太扁,轮廓变化几乎看不见)。
    // 压得比朗伯更狠:侧面要暗到一眼分得出是另一个面,而不是同一面的渐变。
    let shade = 0.10 + 0.90 * pow(clamp(best_facing, 0.0, 1.0), 1.6);
    // 棱边:相邻两面的交界压一道暗线,免得两面糊成一片。
    let edge = smoothstep(0.0, 0.035, abs(abs(best_v) - APOTHEM * 1.1547));
    *dens = clamp(fl * shade, 0.0, 1.0);
    return base * shade * mix(0.35, 1.0, edge);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let fc = frag.xy - u.origin;
    let uv = fc / u.res;
    let aspect = u.res.x / max(u.res.y, 1.0);
    let p = (uv - 0.5) * vec2<f32>(aspect, 1.0);
    let t = u.time * u.speed;

    var col: vec3<f32>;
    var dens = 1.0;

    if (u.variant < 0.5) {
        col = render_ribbon(uv, t) * mix(0.16, 1.0, u.amp);
        col += vec3<f32>(0.020, 0.028, 0.024);
    } else if (u.variant < 1.5) {
        col = render_nebula(uv, p, t) * mix(0.55, 1.0, u.amp);
    } else if (u.variant < 2.5) {
        col = render_fluid(uv, p, t, &dens);
        col = mix(u.col_a.rgb, col, clamp(dens * mix(0.72, 1.0, u.amp), 0.0, 1.0));
    } else if (u.variant < 3.5) {
        col = render_glass(uv, p, t, &dens);
        col = mix(u.col_a.rgb, col, clamp(dens * mix(0.62, 1.0, u.amp), 0.0, 1.0));
    } else if (u.variant > 4.5) {
        col = render_prism(uv, t, &dens);
        col = mix(u.col_a.rgb, col, clamp(dens * mix(0.72, 1.0, u.amp), 0.0, 1.0));
    } else {
        // 进度胶囊:填充段跑流体,未完成段只留极暗底;交界处一条呼吸亮边。
        var fl = 1.0;
        let f = render_fluid(uv, p, t, &fl);
        let head = u.progress;
        let fill = smoothstep(head + 0.004, head - 0.004, uv.x);
        let done = mix(u.col_a.rgb, f, clamp(fl * 0.9 + 0.35, 0.0, 1.0));
        let todo = mix(u.col_a.rgb, u.col_b.rgb, 0.10);
        col = mix(todo, done, fill);
        col += u.col_d.rgb * exp(-pow((uv.x - head) * 26.0, 2.0)) * (0.55 + 0.45 * sin(t * 2.2)) * step(0.001, head);
    }

    // 裁成胶囊 + 边缘一圈极淡的亮线。输出预乘 alpha,Slint 直接合成。
    let hs = u.res * 0.5;
    let sd = sd_round(fc - hs, hs - vec2<f32>(1.0), u.radius);
    let mask = 1.0 - smoothstep(-1.0, 0.6, sd);
    var rim_amt = 0.22;
    if (u.variant > 2.5 && u.variant < 3.5) {
        rim_amt = 0.55;
    }
    let rim = (1.0 - smoothstep(0.0, 1.6, abs(sd + 1.2))) * rim_amt;
    col += vec3<f32>(rim) * mix(u.col_d.rgb, vec3<f32>(1.0), 0.5);
    return vec4<f32>(col * mask, mask);
}
