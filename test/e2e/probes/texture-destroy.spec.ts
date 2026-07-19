// 探查:Slint 每帧销毁多少纹理,以及销毁本身会不会让提交阻塞。
//
// slint 的 femtovg 渲染器在每帧最后一次 flush 之后调 texture_cache.drain()
// (internal/renderers/femtovg/lib.rs:300)。在 WebGPU 上销毁纹理要保证它不再被
// 使用中的提交引用,Dawn 可能因此等到那次提交完成 —— 正好一拍。
//
// 这是 JS 复刻件从没做过的事,也是逐项复刻全部跑满(141/s)之后仅剩的差异之一。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

const CAPTURE = `
  window.__tex = { create: 0, destroy: 0, raf: 0 };
  const ct = GPUDevice.prototype.createTexture;
  GPUDevice.prototype.createTexture = function (...a) { window.__tex.create++; return ct.apply(this, a); };
  const de = GPUTexture.prototype.destroy;
  GPUTexture.prototype.destroy = function (...a) { window.__tex.destroy++; return de.apply(this, a); };
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => raf((t) => { window.__tex.raf++; return cb(t); });
`;

test('线上每帧建/毁多少纹理', async ({ page }) => {
  await page.addInitScript(CAPTURE);
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);
  const a = await page.evaluate(() => ({ ...(window as unknown as { __tex: Record<string, number> }).__tex }));
  await page.waitForTimeout(5000);
  const b = await page.evaluate(() => ({ ...(window as unknown as { __tex: Record<string, number> }).__tex }));
  const frames = b.raf - a.raf;
  console.log(`5 秒 ${frames} 帧`);
  console.log(`  createTexture ${b.create - a.create} 次,${((b.create - a.create) / frames).toFixed(2)} 次/帧`);
  console.log(`  destroy       ${b.destroy - a.destroy} 次,${((b.destroy - a.destroy) / frames).toFixed(2)} 次/帧`);
});

// 因果侧:在裸循环里每帧建一张并销毁,看提交会不会开始阻塞。
for (const perFrame of [0, 1, 4]) {
  test(`裸循环每帧建毁 ${perFrame} 张纹理`, async ({ page }) => {
    await page.goto(`${BASE}/rafprobe.html`);
    await page.evaluate(async (n: number) => {
      const adapter = await navigator.gpu.requestAdapter();
      if (!adapter) throw new Error('没有 WebGPU adapter');
      const dev = await adapter.requestDevice();
      const c = document.createElement('canvas');
      c.width = 1600;
      c.height = 900;
      c.style.width = '1000px';
      c.style.height = '700px';
      document.body.appendChild(c);
      const ctx = c.getContext('webgpu') as GPUCanvasContext | null;
      if (!ctx) throw new Error('拿不到 webgpu context');
      ctx.configure({ device: dev, format: 'rgba8unorm', alphaMode: 'opaque' });
      (function loop() {
        const view = ctx.getCurrentTexture().createView();
        const enc = dev.createCommandEncoder();
        const pass = enc.beginRenderPass({
          colorAttachments: [
            { view, clearValue: { r: 0.1, g: 0.15, b: 0.3, a: 1 }, loadOp: 'clear', storeOp: 'store' },
          ],
        });
        pass.end();
        dev.queue.submit([enc.finish()]);
        // 复刻 texture_cache.drain():提交之后销毁本帧用过的纹理。
        for (let i = 0; i < n; i++) {
          const t = dev.createTexture({ size: [256, 256], format: 'rgba8unorm', usage: 16 | 4 });
          const e2 = dev.createCommandEncoder();
          const p2 = e2.beginRenderPass({
            colorAttachments: [
              { view: t.createView(), clearValue: { r: 0, g: 0, b: 0, a: 1 }, loadOp: 'clear', storeOp: 'store' },
            ],
          });
          p2.end();
          dev.queue.submit([e2.finish()]);
          t.destroy();
        }
        requestAnimationFrame(loop);
      })();
    }, perFrame);
    await page.waitForTimeout(2500);

    const cdp = await page.context().newCDPSession(page);
    const events: Ev[] = [];
    cdp.on('Tracing.dataCollected', (e) => {
      events.push(...(e.value as unknown as Ev[]));
    });
    const done = new Promise<void>((r) => cdp.once('Tracing.tracingComplete', () => r()));
    await cdp.send('Tracing.start', {
      categories: 'toplevel,gpu,viz,disabled-by-default-gpu.dawn,devtools.timeline,disabled-by-default-devtools.timeline',
      transferMode: 'ReportEvents',
    });
    await page.waitForTimeout(4000);
    await cdp.send('Tracing.end');
    await done;
    const per = (n: string) => (events.filter((e) => e.name === n).length / 4).toFixed(1);
    const subs = events
      .filter((e) => e.name === 'Queue::Submit' && e.dur !== undefined)
      .map((e) => (e.dur ?? 0) / 1000)
      .sort((a, b) => a - b);
    console.log(
      `每帧建毁 ${perFrame} 张:  rAF ${per('FireAnimationFrame')}/s  ` +
        `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
        `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
    );
  });
}
