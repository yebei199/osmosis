// 探查:60Hz 是宿主页面带来的,还是 Slint 带来的。
//
// test/hostshape.html 与 apps/web/index.html 的结构、CSS、canvas 元素完全相同,
// 只把 wasm 换成裸 WebGPU 循环。跑到 141/s 说明页面没问题,是 Slint;
// 跑到 60/s 说明宿主页面本身就把帧率钉死了。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

for (const path of ['hostshape.html', 'rafprobe.html']) {
  test(path, async ({ page }) => {
    if (path === 'rafprobe.html') {
      await page.goto(`${BASE}/rafprobe.html`);
      await page.evaluate(async () => {
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
          for (let i = 0; i < 2; i++) {
            const enc = dev.createCommandEncoder();
            if (i === 0) {
              const pass = enc.beginRenderPass({
                colorAttachments: [
                  {
                    view,
                    clearValue: { r: 0.1, g: 0.15, b: 0.3, a: 1 },
                    loadOp: 'clear',
                    storeOp: 'store',
                  },
                ],
              });
              pass.end();
            }
            dev.queue.submit([enc.finish()]);
          }
          requestAnimationFrame(loop);
        })();
      });
    } else {
      await page.goto(`${BASE}/${path}`);
    }
    await page.waitForTimeout(2500);

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

    const per = (n: string) => (events.filter((e) => e.name === n).length / 4).toFixed(1);
    const subs = events
      .filter((e) => e.name === 'Queue::Submit' && e.dur !== undefined)
      .map((e) => (e.dur ?? 0) / 1000)
      .sort((a, b) => a - b);
    console.log(
      `${path.padEnd(18)} rAF ${per('FireAnimationFrame')}/s  ` +
        `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
        `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
    );
  });
}
