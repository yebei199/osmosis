// 探查:取到 swapchain 图像之后一直持有、隔一段工作再提交,会不会掉帧。
//
// 这是复刻清单里唯一的漏网项。slint 在帧的最开头就 getCurrentTexture(lib.rs:153),
// 然后跨过整个 BeforeRendering 回调(带 bevy 时是 13.7ms 的主线程工作)才提交、才 present。
// 之前的 busy-main 把忙碌放在**取图像之前**,顺序不对。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

for (const holdMs of [0, 1.5, 3, 6, 14]) {
  test(`持有 ${holdMs}ms 再提交`, async ({ page }) => {
    await page.goto(`${BASE}/rafprobe.html`);
    await page.evaluate(async (hold: number) => {
      const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
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
        // 复刻 slint 的顺序:先取图像,再干活,最后提交。
        const tex = ctx.getCurrentTexture();
        // 第一次提交:窗口背景 clear(slint 装了 rendering notifier 时的那一次)。
        {
          const enc = dev.createCommandEncoder();
          const pass = enc.beginRenderPass({
            colorAttachments: [
              {
                view: tex.createView(),
                clearValue: { r: 0.1, g: 0.15, b: 0.3, a: 1 },
                loadOp: 'clear',
                storeOp: 'store',
              },
            ],
          });
          pass.end();
          dev.queue.submit([enc.finish()]);
        }
        if (hold > 0) {
          const end = performance.now() + hold;
          while (performance.now() < end);
        }
        // 第二次提交:界面本身。
        {
          const enc = dev.createCommandEncoder();
          const pass = enc.beginRenderPass({
            colorAttachments: [
              { view: tex.createView(), loadOp: 'load', storeOp: 'store' },
            ],
          });
          pass.end();
          dev.queue.submit([enc.finish()]);
        }
        requestAnimationFrame(loop);
      })();
    }, holdMs);
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
      `持有 ${String(holdMs).padStart(4)}ms:  rAF ${per('FireAnimationFrame')}/s  ` +
        `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
        `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
    );
  });
}
