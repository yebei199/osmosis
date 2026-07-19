// 探查:device 本身是不是那个差别。
//
// 尺寸、格式、alphaMode、离屏拷贝、多次提交、提前取图像全部复刻过,裸画布都是 141/s
// (见 format-swap、path-shape)。只剩 render3d 建 device 时申请的那套 limits/features。
//
// 描述符不写死:先在应用页上拦下真实的 requestDevice,再拿它去对照页建 device。写死的
// 副本会过期,而过期了不会报错,只会让结论悄悄失效。
//
// 之前那轮同样的实验量的是 rAF 间隔,这里改看 Display::DrawAndSwap —— 两者不是一回事。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

const CAPTURE = `
  window.__descs = [];
  const orig = GPUAdapter.prototype.requestDevice;
  GPUAdapter.prototype.requestDevice = function (desc) {
    window.__descs.push(JSON.parse(JSON.stringify(desc ?? {})));
    return orig.call(this, desc);
  };
`;

const LOOP = `
  (async (desc) => {
    const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
    const dev = await adapter.requestDevice(desc);
    const lost = await Promise.race([
      dev.lost.then(() => true),
      new Promise((r) => setTimeout(() => r(false), 200)),
    ]);
    if (lost) { window.__built = '出生即丢失'; return; }
    const c = document.createElement('canvas');
    c.width = 1600; c.height = 1125;
    c.style.width = '1280px'; c.style.height = '900px';
    document.body.appendChild(c);
    const ctx = c.getContext('webgpu');
    ctx.configure({ device: dev, format: 'rgba8unorm', alphaMode: 'opaque' });
    (function loop() {
      const enc = dev.createCommandEncoder();
      const pass = enc.beginRenderPass({ colorAttachments: [{
        view: ctx.getCurrentTexture().createView(),
        clearValue: { r: 0.1, g: 0.15, b: 0.3, a: 1 }, loadOp: 'clear', storeOp: 'store',
      }]});
      pass.end();
      dev.queue.submit([enc.finish()]);
      requestAnimationFrame(loop);
    })();
    window.__built = 'ok';
  })
`;

async function measure(page: import('@playwright/test').Page, label: string) {
  const cdp = await page.context().newCDPSession(page);
  const events: Ev[] = [];
  cdp.on('Tracing.dataCollected', (e) => {
    events.push(...(e.value as unknown as Ev[]));
  });
  const done = new Promise<void>((r) =>
    cdp.once('Tracing.tracingComplete', () => r()),
  );
  await cdp.send('Tracing.start', {
    categories:
      'toplevel,gpu,viz,disabled-by-default-gpu.dawn,devtools.timeline,disabled-by-default-devtools.timeline',
    transferMode: 'ReportEvents',
  });
  await page.waitForTimeout(4000);
  await cdp.send('Tracing.end');
  await done;
  await cdp.detach();

  const per = (n: string) => (events.filter((e) => e.name === n).length / 4).toFixed(1);
  const subs = events
    .filter((e) => e.name === 'Queue::Submit' && e.dur !== undefined)
    .map((e) => (e.dur ?? 0) / 1000)
    .sort((a, b) => a - b);
  console.log(
    `${label.padEnd(18)} rAF ${per('FireAnimationFrame')}/s  ` +
      `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
      `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
  );
}

test('默认 device 对 render3d 的 device', async ({ page }) => {
  // ── 先在应用页拦下真实描述符 ──
  await page.addInitScript(CAPTURE);
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);
  const descs = await page.evaluate(
    () => (window as unknown as { __descs: unknown[] }).__descs,
  );
  console.log(`拦到 ${descs.length} 个 requestDevice 描述符`);
  const real = descs[descs.length - 1];
  console.log(`最后一个: ${JSON.stringify(real).slice(0, 400)}`);

  // ── 拿它去对照页建 device,跑同一个循环 ──
  for (const [label, desc] of [
    ['默认 device', undefined],
    ['render3d device', real],
  ] as const) {
    await page.goto(`${BASE}/rafprobe.html`);
    const built = await page.evaluate(
      async ({ loop, d }) => {
        // biome-ignore lint/security/noGlobalEval: 探针要在页面上下文里注入一段循环
        await (0, eval)(loop)(d);
        return (window as unknown as { __built: string }).__built;
      },
      { loop: LOOP, d: desc },
    );
    if (built !== 'ok') {
      console.log(`${label}: ${built}`);
      continue;
    }
    await page.waitForTimeout(2000);
    await measure(page, label);
  }
});
