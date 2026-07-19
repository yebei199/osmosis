// 探查:主线程每帧忙多久会把呈现帧率打对折。
//
// WebGPU 那一侧已经查干净了:画布配置、device、渲染路径形状、每帧调用序列全部复刻过,
// 裸画布都是 141/s 且提交不阻塞,而应用页每帧的 WebGPU 调用比复刻件还少。
// 剩下的唯一变量是回调里那段 wasm 占掉的主线程时间。
//
// 注意量的是 Display::DrawAndSwap 不是 rAF —— 之前那轮"每帧 1ms JS 无影响"量的是 rAF。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

for (const busyMs of [0, 1, 3, 6]) {
  test(`每帧忙 ${busyMs}ms`, async ({ page }) => {
    await page.goto(`${BASE}/rafprobe.html`);
    await page.evaluate(async (busy: number) => {
      const adapter = await navigator.gpu.requestAdapter();
      if (!adapter) throw new Error('没有 WebGPU adapter');
      const dev = await adapter.requestDevice();
      const c = document.createElement('canvas');
      c.width = 1600;
      c.height = 900;
      c.style.width = '1280px';
      c.style.height = '720px';
      document.body.appendChild(c);
      const ctx = c.getContext('webgpu') as GPUCanvasContext | null;
      if (!ctx) throw new Error('拿不到 webgpu context');
      ctx.configure({ device: dev, format: 'rgba8unorm', alphaMode: 'opaque' });
      (function loop() {
        if (busy > 0) {
          const end = performance.now() + busy;
          while (performance.now() < end);
        }
        // 复刻应用页实测的调用序列:1 次 getCurrentTexture,2 次 submit。
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
    }, busyMs);
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
      `忙 ${busyMs}ms:  rAF ${per('FireAnimationFrame')}/s  ` +
        `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
        `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
    );
  });
}
