// 探查:两次提交都往画布纹理上画,是不是那个差别。
//
// Slint 的一帧是:先 flush 一条窗口背景 clear(第一次提交),再 flush 整个界面
// (第二次提交),两次都以 swapchain 图像为目标。之前的复刻件第二次提交是空的。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

const CASES = [
  { name: '第二次空提交', bothDraw: false },
  { name: '两次都画(=Slint)', bothDraw: true },
];

for (const c of CASES) {
  test(c.name, async ({ page }) => {
    await page.goto(`${BASE}/rafprobe.html`);
    await page.evaluate(async (bothDraw: boolean) => {
      const adapter = await navigator.gpu.requestAdapter();
      if (!adapter) throw new Error('没有 WebGPU adapter');
      const dev = await adapter.requestDevice();
      const c2 = document.createElement('canvas');
      c2.width = 1600;
      c2.height = 900;
      c2.style.width = '1000px';
      c2.style.height = '700px';
      document.body.appendChild(c2);
      const ctx = c2.getContext('webgpu') as GPUCanvasContext | null;
      if (!ctx) throw new Error('拿不到 webgpu context');
      ctx.configure({ device: dev, format: 'rgba8unorm', alphaMode: 'opaque' });
      (function loop() {
        const view = ctx.getCurrentTexture().createView();
        for (let i = 0; i < 2; i++) {
          const enc = dev.createCommandEncoder();
          if (i === 0 || bothDraw) {
            const pass = enc.beginRenderPass({
              colorAttachments: [
                {
                  view,
                  clearValue: { r: 0.1, g: 0.15, b: 0.3, a: 1 },
                  // 第二遍走 load:界面画在已经清好的背景上。
                  loadOp: i === 0 ? 'clear' : 'load',
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
    }, c.bothDraw);
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
      `${c.name.padEnd(20)} rAF ${per('FireAnimationFrame')}/s  ` +
        `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
        `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
    );
  });
}
