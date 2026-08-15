// 极光光带按钮：多条沿正弦流场无限流动的光带（bands 属性，默认 3），逐通道色散 + 方向性动态模糊。
// 这里用 WebGL 跑，数学与 WGSL 版一一对应（见 design_handoff/shaders.md §9）。
(function () {
  const VERT = `attribute vec2 a;void main(){ gl_Position = vec4(a, 0.0, 1.0); }`;

  const FRAG = `precision highp float;
uniform vec2  uRes;
uniform float uTime;
uniform float uSeed;
uniform float uSpeed;
uniform float uAmp;      // 静息 → 悬停
uniform float uMode;     // 0 = 全光谱, 1 = 绿色板
uniform float uRadius;
uniform float uBands;    // ribbon 的光带条数 1..4
uniform float uVariant;  // 0 ribbon 1 nebula 2 fluid 3 glass 4 progress
uniform float uProgress; // progress 变体的进度 0..1
uniform vec2  uPointer;  // 归一化指针位置
uniform vec3  uA;        // 底色
uniform vec3  uB;        // 主色
uniform vec3  uC;        // 次色
uniform vec3  uD;        // 高光

float sdRound(vec2 p, vec2 b, float r){
  vec2 q = abs(p) - b + r;
  return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}
vec3 hsv(float h, float s, float v){
  vec3 k = fract(vec3(h) + vec3(0.0, 2.0/3.0, 1.0/3.0)) * 6.0 - 3.0;
  return v * mix(vec3(1.0), clamp(abs(k) - 1.0, 0.0, 1.0), s);
}
float hash21(vec2 p){
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32 + uSeed);
  return fract(p.x * p.y);
}
float noise(vec2 p){
  vec2 i = floor(p), f = fract(p);
  f = f * f * (3.0 - 2.0 * f);
  return mix(mix(hash21(i), hash21(i + vec2(1.0,0.0)), f.x),
             mix(hash21(i + vec2(0.0,1.0)), hash21(i + vec2(1.0,1.0)), f.x), f.y);
}
// 四阶 fbm —— 按钮尺寸小，四阶足够，六阶只是白白烧 GPU
float fbm(vec2 p){
  float v = 0.0, a = 0.53;
  mat2 rot = mat2(0.80, -0.60, 0.60, 0.80);
  for (int i = 0; i < 4; i++){
    v += a * noise(p);
    p = rot * p * 2.02 + vec2(17.13, 9.27);
    a *= 0.49;
  }
  return v;
}
float gaussian(float x, float c, float w){ return exp(-pow(x - c, 2.0) / max(w, 0.0001)); }
float softBlob(vec2 p, vec2 c, float r, float soft){ return 1.0 - smoothstep(r - soft, r + soft, length(p - c)); }

// ── 变体 0：光带 ────────────────────────────────────────────
float centerAt(float x, float t, float s, float sp){
  return 0.50
    + 0.150*sin(6.2831*(x*0.80) + t*0.60*sp + s)
    + 0.085*sin(6.2831*(x*1.70) - t*0.45*sp + s*1.7)
    + 0.045*sin(6.2831*(x*3.10) + t*0.90*sp + s*0.6);
}
float widthAt(float x, float t, float s, float sp){
  return (0.052 + 0.030*sin(6.2831*(x*1.30) - t*0.50*sp + s)) * mix(1.0, 0.58, clamp((uBands-1.0)/3.0, 0.0, 1.0));
}
float bandAt(float y, float c, float w, float dy){
  float d = (y - c - dy) / w;
  return exp(-d*d*2.2);
}
vec3 renderRibbon(vec2 p, float t){
  vec3 col = vec3(0.0);

  // 每条带跑一遍色散 + 动态模糊，再相加
  for (int i = 0; i < 4; i++){
    float fi = float(i);
    float on = step(fi + 0.5, uBands);   // 超出条数的直接乘 0
    float sd2 = uSeed + fi * 2.37;       // 相位错开
    float sp = 1.0 + fi * 0.16;          // 流速错开
    float bw = 1.0 - fi * 0.15;          // 后面的带压暗，做出前后层次

    // 斜率越大色散越强 —— 光带拐弯的地方才出彩虹边
    float h = 0.004;
    float slope = abs(centerAt(p.x + h, t, sd2, sp) - centerAt(p.x - h, t, sd2, sp)) / (2.0*h);
    float disp = (0.006 + 0.030 * clamp(slope*0.16, 0.0, 1.0)) * mix(0.55, 1.0, uAmp);

    // 方向性动态模糊：沿流动方向取 6 个样本，每个样本只求一次流场
    vec3 acc = vec3(0.0);
    float core = 0.0;
    for (int k = 0; k < 6; k++){
      float fk = float(k);
      float wgt = 1.0 - fk/7.0;
      float xo = p.x - fk * 0.0060;
      float c0 = centerAt(xo, t, sd2, sp);
      float w0 = widthAt(xo, t, sd2, sp);
      acc += vec3(bandAt(p.y, c0, w0, -disp), bandAt(p.y, c0, w0, 0.0), bandAt(p.y, c0, w0, disp)) * wgt;
      core += bandAt(p.y, c0, w0, 0.0) * wgt;
    }
    acc /= 3.9; core /= 3.9;

    // 外圈辉光
    float glow = exp(-pow((p.y - centerAt(p.x,t,sd2,sp)) / (widthAt(p.x,t,sd2,sp)*3.4), 2.0)) * 0.26;

    vec3 tint;
    if (uMode < 0.5){
      // 全光谱：色相沿 x 与时间推移，每条带再错开一段
      tint = hsv(fract(p.x*0.85 + t*0.07 + core*0.22 + fi*0.21), 0.85, 1.0);
    } else {
      // 收进绿色板：主绿 → 青 → 黄绿，边缘留一点紫做色散
      float hh = 0.30 + 0.13*sin(6.2831*(p.x*0.7) + t*0.25 + fi*1.10) + core*0.05 + fi*0.035;
      tint = hsv(hh, 0.68, 1.0);
      tint = mix(tint, vec3(0.62,0.56,0.92), clamp(slope*0.05,0.0,0.35)*0.6);
    }

    vec3 c = acc * tint * 1.75;
    c += vec3(glow) * tint * 0.7;
    c += vec3(pow(core, 3.0)) * 1.9;           // 白热核心
    col += c * bw * on;
  }

  col /= (0.62 + uBands * 0.30);               // 条数越多整体越收，避免糊成一片白
  col *= mix(0.16, 1.0, uAmp);                 // 静息态只留一成多亮度


  col /= (0.62 + uBands * 0.30);
  return col;
}

// ── 变体 1：星云（域扭曲 fbm + 星点，取自 NC-01～06 的做法）──
vec3 renderNebula(vec2 uv, vec2 p, float t){
  vec2 ptr = (uPointer - 0.5) * vec2(uRes.x / max(uRes.y, 1.0), 1.0);
  vec2 delta = p - ptr;
  float dToP = length(delta);
  float infl = exp(-dToP * 4.6) * uAmp;
  float ang = infl * 1.7;
  mat2 swirl = mat2(cos(ang), -sin(ang), sin(ang), cos(ang));
  p = ptr + swirl * delta + normalize(delta + 0.0001) * infl * 0.08;

  vec2 drift = vec2(t * 0.22, -t * 0.13);
  vec2 q = vec2(fbm(p * 1.35 + drift + uSeed), fbm(p * 1.35 + vec2(5.2, 1.3) - drift * 0.85));
  vec2 r = vec2(fbm(p * 2.0 + 3.6 * q + t * 0.10), fbm(p * 2.0 + 3.0 * q - t * 0.085));
  float cloud = fbm(p * 1.7 + 4.2 * r);
  float veins = fbm(p * 4.0 - 2.0 * q + t * 0.065);
  float n = smoothstep(0.18, 0.91, cloud * 0.9 + veins * 0.22);

  vec3 col = mix(mix(uA, uB, smoothstep(0.06, 0.62, n)), mix(uB, uC, smoothstep(0.30, 0.82, n)), smoothstep(0.26, 0.72, n));
  col = mix(col, uD, smoothstep(0.80, 0.98, n));
  col += uD * pow(max(cloud - 0.63, 0.0), 2.0) * 1.05;
  col *= 0.78 + 0.34 * smoothstep(0.15, 0.9, veins);

  // 星点：格子里挑万分之一的格子点亮，再各自闪烁
  vec2 cell = fract(uv * vec2(96.0, 42.0)) - 0.5;
  float rnd = hash21(floor(uv * vec2(96.0, 42.0)));
  float star = step(0.988, rnd) * smoothstep(0.085, 0.0, length(cell));
  col += star * (0.35 + 0.65 * (0.5 + 0.5 * sin(t * (1.0 + rnd * 2.4) + rnd * 40.0))) * mix(uC, uD, rnd);
  col += uD * exp(-dToP * 7.0) * uAmp * 0.28;
  return col;
}

// ── 变体 2：流体玻璃（左侧留白给文字，流体压在右侧）────────
vec3 renderFluid(vec2 uv, vec2 p, float t, out float dens){
  vec2 ptr = (uPointer - 0.5) * vec2(uRes.x / max(uRes.y, 1.0), 1.0);
  vec2 dl = p - ptr;
  float d = length(dl);
  float field = exp(-d * d * 7.2) * uAmp;
  vec2 nrm = dl / max(d, 0.035);
  p += nrm * field * 0.115;

  vec2 sv = vec2(uSeed * 1.713, uSeed * 0.937);
  float w1 = fbm(p * 1.22 + sv + vec2(t * 0.075, -t * 0.052));
  float w2 = fbm(p * 1.54 - sv * 0.37 + vec2(-t * 0.057, t * 0.064) + w1 * 0.82);
  vec2 q = p + (vec2(w1, w2) - 0.5) * 0.58;
  float broad = fbm(q * 1.12 + vec2(t * 0.041, -t * 0.033));
  float detail = fbm(q * 2.18 + vec2(-t * 0.083, t * 0.057) + broad * 0.95);
  float ribbonN = 0.5 + 0.5 * sin(q.x * 3.15 + q.y * 0.76 + detail * 5.0 + t * 0.25 + uSeed);

  vec3 fluid = mix(uB, uC, smoothstep(0.16, 0.88, broad * 0.61 + ribbonN * 0.39));
  fluid = mix(fluid, uA, smoothstep(0.43, 0.84, detail * 0.69 + (0.5 + 0.5 * sin(q.y * 4.2 - q.x * 0.8 - t * 0.17)) * 0.31) * 0.74);

  float aspect = uRes.x / max(uRes.y, 1.0);
  float haze = clamp(softBlob(p, vec2(aspect * 0.23 + 0.12 * sin(t * 0.08 + uSeed), 0.16 * cos(t * 0.11 + uSeed)), 0.52, 0.38) * 0.72
                   + softBlob(p, vec2(aspect * 0.39 + 0.10 * cos(t * 0.07 - uSeed), -0.24 + 0.11 * sin(t * 0.09)), 0.43, 0.34) * 0.58, 0.0, 1.0);
  // reveal：左侧接近 0，右侧才让流体出现 —— 文字区不被糊住
  float reveal = clamp(smoothstep(0.055, 0.735, uv.x + (0.5 - broad) * 0.27 + 0.070 * sin(uv.y * 4.0 + t * 0.12)) * mix(0.70, 1.0, haze), 0.0, 1.0);

  float spec = pow(clamp(1.0 - abs(detail - 0.52) * 2.0, 0.0, 1.0), 5.0) * reveal;
  float caustic = pow(clamp(0.52 + 0.48 * sin((q.x - q.y) * 5.2 + detail * 7.0 - t * 0.18), 0.0, 1.0), 7.0) * reveal;
  vec3 col = mix(fluid, uD, spec * 0.20 + caustic * 0.10);
  col *= 0.78 + 0.25 * haze;
  float filament = smoothstep(0.48, 0.86, detail) * reveal;
  dens = clamp(reveal * (0.36 + 0.48 * haze) + filament * 0.22 + field * 0.28, 0.0, 1.0);
  return col;
}

// ── 变体 3：液态玻璃（几乎透明，只留折射亮边与焦散）────────
vec3 renderGlass(vec2 uv, vec2 p, float t, out float dens){
  vec2 sv = vec2(uSeed * 1.2, uSeed * 0.7);
  float w = fbm(p * 1.9 + sv + vec2(t * 0.05, -t * 0.04));
  vec2 q = p + (vec2(w, fbm(p * 2.2 - sv + t * 0.03)) - 0.5) * 0.32;
  float detail = fbm(q * 2.6 + t * 0.05);
  float caustic = pow(clamp(0.52 + 0.48 * sin((q.x - q.y) * 6.4 + detail * 8.0 - t * 0.22), 0.0, 1.0), 8.0);
  float spec = pow(clamp(1.0 - abs(detail - 0.5) * 2.2, 0.0, 1.0), 6.0);
  float sheen = smoothstep(0.35, 0.0, abs(uv.y - (0.30 + 0.10 * sin(t * 0.4)))) * 0.35;
  vec3 col = mix(uA, uB, detail * 0.5) + uD * (spec * 0.55 + caustic * 0.40) + uC * sheen * 0.5;
  dens = clamp(0.14 + spec * 0.5 + caustic * 0.45 + sheen * 0.4, 0.0, 1.0);
  return col;
}

void main(){
  vec2 fc = gl_FragCoord.xy;
  vec2 uv = fc / uRes;
  float aspect = uRes.x / max(uRes.y, 1.0);
  vec2 p = (uv - 0.5) * vec2(aspect, 1.0);
  float t = uTime * uSpeed;

  vec3 col;
  float dens = 1.0;

  if (uVariant < 0.5){
    col = renderRibbon(uv, t) * mix(0.16, 1.0, uAmp);
    col += vec3(0.020, 0.028, 0.024);
  } else if (uVariant < 1.5){
    col = renderNebula(uv, p, t) * mix(0.55, 1.0, uAmp);
  } else if (uVariant < 2.5){
    col = renderFluid(uv, p, t, dens);
    col = mix(uA, col, clamp(dens * mix(0.72, 1.0, uAmp), 0.0, 1.0));
  } else if (uVariant < 3.5){
    col = renderGlass(uv, p, t, dens);
    col = mix(uA, col, clamp(dens * mix(0.62, 1.0, uAmp), 0.0, 1.0));
  } else {
    // 进度胶囊：填充段跑流体，未完成段只留极暗底；交界处一条亮边
    float fl;
    vec3 f = renderFluid(uv, p, t, fl);
    float head = uProgress;
    float fill = smoothstep(head + 0.004, head - 0.004, uv.x);
    vec3 done = mix(uA, f, clamp(fl * 0.9 + 0.35, 0.0, 1.0));
    vec3 todo = mix(uA, uB, 0.10);
    col = mix(todo, done, fill);
    col += uD * exp(-pow((uv.x - head) * 26.0, 2.0)) * (0.55 + 0.45 * sin(t * 2.2)) * step(0.001, head);
  }

  // 裁成胶囊 + 边缘一圈极淡的亮线
  vec2 hs = uRes * 0.5;
  float sd = sdRound(fc - hs, hs - 1.0, uRadius);
  float mask = 1.0 - smoothstep(-1.0, 0.6, sd);
  float rim = (1.0 - smoothstep(0.0, 1.6, abs(sd + 1.2))) * (uVariant > 2.5 && uVariant < 3.5 ? 0.55 : 0.22);
  col += vec3(rim) * mix(uD, vec3(1.0), 0.5);
  gl_FragColor = vec4(col * mask, mask);
}`;


  function compile(gl, type, src) {
    const sh = gl.createShader(type);
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
      console.error("[aurora-button] shader compile failed:", gl.getShaderInfoLog(sh));
      return null;
    }
    return sh;
  }

  // ── 合批渲染器：整页只有一个 WebGL context ──────────────────────────
  // 每颗按钮自己是一张 2D canvas；共享 context 逐颗画进离屏缓冲再 drawImage 过去。
  // 与真机结论一致：光带是一个纹理 pass，必须合批，不能一颗一个 context。
  const R = {
    cv: null, gl: null, U: null, W: 0, H: 0,
    items: new Set(), pending: false, _raf: 0, _timer: 0, t0: 0, last: 0, frames: 0, cost: 0, dead: false,

    // 预览宿主里 document 可能一直是 hidden、rAF 永不触发，所以让 rAF 和定时器竞速，谁先到谁跑
    schedule() {
      if (this.pending || this.dead) return;
      this.pending = true;
      const run = () => {
        if (!R.pending) return;
        R.pending = false;
        // 输掉竞速的那个必须取消，否则迟到的回调会消费下一轮的 pending，回调数逐轮翻倍
        cancelAnimationFrame(R._raf);
        clearTimeout(R._timer);
        R.tick();
      };
      this._raf = requestAnimationFrame(run);
      this._timer = setTimeout(run, 42);
    },

    init() {
      if (this.gl) return this.gl;
      if (this.dead) return null;
      const cv = document.createElement("canvas");
      cv.width = cv.height = 1;
      const gl = cv.getContext("webgl", { alpha: true, premultipliedAlpha: true, antialias: false, preserveDrawingBuffer: false });
      if (!gl) { this.dead = true; return null; }
      const vs = compile(gl, gl.VERTEX_SHADER, VERT);
      const fs = compile(gl, gl.FRAGMENT_SHADER, FRAG);
      if (!vs || !fs) { this.dead = true; return null; }
      const prog = gl.createProgram();
      gl.attachShader(prog, vs);
      gl.attachShader(prog, fs);
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
        console.error("[aurora-button] link failed:", gl.getProgramInfoLog(prog));
        this.dead = true; return null;
      }
      gl.useProgram(prog);
      const buf = gl.createBuffer();
      gl.bindBuffer(gl.ARRAY_BUFFER, buf);
      gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
      const loc = gl.getAttribLocation(prog, "a");
      gl.enableVertexAttribArray(loc);
      gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
      const u = (n) => gl.getUniformLocation(prog, n);
      this.cv = cv; this.gl = gl;
      this.U = { res: u("uRes"), time: u("uTime"), seed: u("uSeed"), speed: u("uSpeed"), amp: u("uAmp"),
                 mode: u("uMode"), radius: u("uRadius"), bands: u("uBands"), variant: u("uVariant"),
                 progress: u("uProgress"), pointer: u("uPointer"), a: u("uA"), b: u("uB"), c: u("uC"), d: u("uD") };
      gl.enable(gl.SCISSOR_TEST);
      this.t0 = performance.now();
      return gl;
    },

    fit(w, h) {
      if (w <= this.W && h <= this.H) return;
      this.W = Math.max(this.W, w); this.H = Math.max(this.H, h);
      this.cv.width = this.W; this.cv.height = this.H;
    },

    add(it) {
      if (!this.init()) { cssFallback(it); return; }
      this.fit(it.pw, it.ph);
      this.items.add(it);
      this.schedule();
    },

    remove(it) { this.items.delete(it); },

    // GPU 不给力（软件渲染 / 集显）时整体退回 CSS，别把主线程拖垮
    kill(why) {
      if (this.dead) return;
      this.dead = true;
      console.warn("[aurora-button] " + why + " —— 全部退到 CSS 兜底");
      this.pending = false;
      cancelAnimationFrame(this._raf);
      clearTimeout(this._timer);
      const list = [...this.items];
      this.items.clear();
      list.forEach((it) => { if (it.cv && it.cv.parentNode) it.cv.remove(); cssFallback(it); });
      this.gl = null;
    },

    tick: null,
  };

  R.tick = () => {
    if (R.dead) return;
    const now = performance.now();
    const gl = R.gl, U = R.U;

    let anyHot = false;
    R.items.forEach((it) => {
      if (Math.abs(it.target - it.amp) > 0.004 || it.target > it.rest + 0.001) anyHot = true;
      if (Math.abs(it.ptx - it.px) > 0.004 || Math.abs(it.pty - it.py) > 0.004) anyHot = true;
    });
    // 全部静息时降到 ~20fps；有一颗在过渡/悬停就满帧
    if (!anyHot && now - R.last < 48) { R.schedule(); return; }
    R.last = now;

    const t0f = now;
    const t = (now - R.t0) / 1000;
    R.items.forEach((it) => {
      if (!it.visible) return;
      it.amp += (it.target - it.amp) * 0.09;
      gl.viewport(0, 0, it.pw, it.ph);
      gl.scissor(0, 0, it.pw, it.ph);
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      gl.uniform2f(U.res, it.pw, it.ph);
      gl.uniform1f(U.time, t);
      gl.uniform1f(U.seed, it.seed);
      gl.uniform1f(U.speed, it.speed);
      gl.uniform1f(U.amp, it.amp);
      gl.uniform1f(U.mode, it.mode);
      gl.uniform1f(U.radius, it.radius);
      gl.uniform1f(U.bands, it.bands);
      gl.uniform1f(U.variant, it.variant);
      gl.uniform1f(U.progress, it.progress);
      it.px += (it.ptx - it.px) * 0.10;
      it.py += (it.pty - it.py) * 0.10;
      gl.uniform2f(U.pointer, it.px, it.py);
      gl.uniform3fv(U.a, it.cols[0]);
      gl.uniform3fv(U.b, it.cols[1]);
      gl.uniform3fv(U.c, it.cols[2]);
      gl.uniform3fv(U.d, it.cols[3]);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      it.ctx.drawImage(R.cv, 0, R.H - it.ph, it.pw, it.ph, 0, 0, it.pw, it.ph);
    });

    // 前 10 帧测一次开销：一帧画完所有按钮不该超过 ~18ms
    if (R.frames < 10) {
      R.cost += performance.now() - t0f;
      if (++R.frames === 10 && R.cost / 10 > 18) { R.kill("GPU 太慢（每帧 " + (R.cost / 10).toFixed(1) + "ms）"); return; }
    }

    R.schedule();
  };

  // 拿不到 WebGL 时的兜底：用模糊渐变条近似同一套流场,条数、色板、静息亮度都对齐
  let fbStyled = false;
  function cssFallback(it) {
    const el = it.el, n = it.bands, green = it.mode > 0.5;
    if (!fbStyled) {
      fbStyled = true;
      const st = document.createElement("style");
      st.textContent = "@keyframes ab-flow{0%{transform:translate(-6%,-42%) rotate(-3.2deg) scaleY(1)}" +
        "50%{transform:translate(6%,38%) rotate(3.4deg) scaleY(1.5)}" +
        "100%{transform:translate(-6%,-42%) rotate(-3.2deg) scaleY(1)}}";
      document.head.appendChild(st);
    }
    el.style.background = "#080d0b";
    // shader 的 rest 靠白热核心撑亮度,模糊渐变没有,所以兜底另设下限
    const restOp = Math.max(it.rest, 0.42);
    const wrap = document.createElement("div");
    wrap.style.cssText = "position:absolute;inset:0;overflow:hidden;filter:blur(7px) saturate(1.35);opacity:" + restOp + ";transition:opacity .28s ease";
    for (let i = 0; i < n; i++) {
      const b = document.createElement("div");
      const hue = green ? 96 + i * 26 : i * 78 + 20;
      const grad = green
        ? "linear-gradient(90deg,transparent,hsl(" + hue + " 62% 58%),hsl(" + (hue + 34) + " 70% 70%),hsl(272 55% 66%),transparent)"
        : "linear-gradient(90deg,transparent,hsl(" + hue + " 90% 60%),hsl(" + (hue + 70) + " 90% 66%),hsl(" + (hue + 150) + " 90% 62%),transparent)";
      b.style.cssText = "position:absolute;left:-14%;right:-14%;top:50%;height:" + (14 - i * 1.6) + "%;" +
        "border-radius:50%;background:" + grad + ";mix-blend-mode:screen;" +
        "animation:ab-flow " + (5.2 + i * 1.35) + "s ease-in-out " + (i * -1.7) + "s infinite";
      wrap.appendChild(b);
      // 常亮核心线:对应 shader 里的 pow(core,3.0) 白热芯,静息时保证可读
      const core = document.createElement("div");
      core.style.cssText = "position:absolute;left:-14%;right:-14%;top:50%;height:" + Math.max(1.6, 4 - i * 0.7) + "%;" +
        "border-radius:50%;background:linear-gradient(90deg,transparent,rgba(255,255,255,.92),rgba(255,255,255,.55),transparent);" +
        "mix-blend-mode:screen;filter:blur(1px);" +
        "animation:ab-flow " + (5.2 + i * 1.35) + "s ease-in-out " + (i * -1.7) + "s infinite";
      wrap.appendChild(core);
    }
    el.insertBefore(wrap, el.firstChild);
    el.addEventListener("pointerenter", () => { wrap.style.opacity = "1"; });
    el.addEventListener("pointerleave", () => { wrap.style.opacity = String(restOp); });
  }

  class AuroraButton extends HTMLElement {
    connectedCallback() {
      if (this._built) return;
      this._built = true;
      const w = +(this.getAttribute("w") || 240);
      const h = +(this.getAttribute("h") || 76);
      const radius = this.getAttribute("radius") ? +this.getAttribute("radius") : h / 2;
      const label = this.getAttribute("label") || "";
      const mode = this.getAttribute("mode") === "green" ? 1 : 0;
      const speed = +(this.getAttribute("speed") || 1);
      const seed = +(this.getAttribute("seed") || 0);
      const bands = Math.max(1, Math.min(4, +(this.getAttribute("bands") || 3)));
      const VARIANTS = { ribbon: 0, nebula: 1, fluid: 2, glass: 3, progress: 4 };
      const variant = VARIANTS[this.getAttribute("variant") || "ribbon"] || 0;
      const progress = Math.max(0, Math.min(1, +(this.getAttribute("progress") || 0.62)));
      // 四色板:底 → 主 → 次 → 高光。默认跟 Osmosis 绿板走,spectrum 只影响 ribbon 的色相环
      const DEF = mode === 1 || variant > 0
        ? ["#0b1310", "#4f7a3f", "#8fc46a", "#e9f7d6"]
        : ["#080d0b", "#3d6b2f", "#8fc46a", "#eaf3e2"];
      const cols = (this.getAttribute("colors") || DEF.join(",")).split(",").map((h) => {
        const v = parseInt(h.trim().replace("#", ""), 16);
        return new Float32Array([((v >> 16) & 255) / 255, ((v >> 8) & 255) / 255, (v & 255) / 255]);
      });
      while (cols.length < 4) cols.push(cols[cols.length - 1]);
      const restAttr = this.getAttribute("rest");
      const rest = restAttr === "false" ? 1 : (restAttr !== null && !isNaN(parseFloat(restAttr)) ? parseFloat(restAttr) : 0.12);

      this.style.cssText = `position:relative;display:inline-grid;place-items:center;width:${w}px;height:${h}px;border-radius:${radius}px;overflow:hidden;cursor:pointer`;

      const dpr = Math.min(window.devicePixelRatio || 1, 1);
      const cv = document.createElement("canvas");
      cv.width = Math.round(w * dpr);
      cv.height = Math.round(h * dpr);
      cv.style.cssText = "position:absolute;inset:0;width:100%;height:100%;display:block";
      this.appendChild(cv);

      if (label) {
        const sp = document.createElement("span");
        sp.textContent = label;
        const ink = this.getAttribute("ink") || "#fff";
        const align = variant >= 2 ? "flex-start" : "center";
        this.style.justifyItems = align;
        sp.style.cssText = `position:relative;font:700 ${Math.round(h * 0.30)}px Figtree,system-ui,sans-serif;color:${ink};letter-spacing:.01em;text-shadow:0 2px 14px #0009;pointer-events:none;padding:0 ${Math.round(h * 0.34)}px`;
        this.appendChild(sp);
      }

      const ctx = cv.getContext("2d");
      ctx.globalCompositeOperation = "copy";

      const it = this._it = {
        el: this, ctx, cv, pw: cv.width, ph: cv.height,
        seed, speed, mode, bands, rest, radius: radius * dpr,
        variant, progress, cols,
        px: 0.72, py: 0.5, ptx: 0.72, pty: 0.5,
        amp: rest, target: rest, visible: true,
      };

      this.addEventListener("pointerenter", () => { it.target = 1; });
      this.addEventListener("pointerleave", () => { it.target = rest; it.ptx = 0.72; it.pty = 0.5; });
      this.addEventListener("pointermove", (ev) => {
        const r = this.getBoundingClientRect();
        it.ptx = (ev.clientX - r.left) / Math.max(r.width, 1);
        it.pty = 1 - (ev.clientY - r.top) / Math.max(r.height, 1);
      }, { passive: true });

      // 进度可外部改:el.progress = 0.35
      Object.defineProperty(this, "progress", {
        get: () => it.progress,
        set: (v) => { it.progress = Math.max(0, Math.min(1, +v || 0)); },
        configurable: true,
      });

      // 省电门:滚出视口就不画,与仓库既有取向一致
      if (window.IntersectionObserver) {
        it.io = new IntersectionObserver((es) => { it.visible = es[0].isIntersecting; }, { threshold: 0.01 });
        it.io.observe(this);
      }

      R.add(it);
    }

    disconnectedCallback() {
      if (this._it) { R.remove(this._it); if (this._it.io) this._it.io.disconnect(); }
      this._built = false;
    }
  }

  if (!customElements.get("aurora-button")) customElements.define("aurora-button", AuroraButton);
})();
