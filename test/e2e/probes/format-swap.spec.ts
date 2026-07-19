// 复核:canvas 格式对**真正呈现出去的帧数**有没有影响。
//
// 之前那轮"逐项复刻 canvas 配置,全 145Hz"量的是 rAF 间隔,而 rAF 频率不等于呈现帧数
// (实测过应用页 rAF 106/s、DrawAndSwap 只有 53/s)。这里改看 Display::DrawAndSwap。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

const CASES = [
  { name: 'bgra8unorm(Chrome 首选)', format: 'bgra8unorm', alpha: 'opaque' },
  { name: 'rgba8unorm(=Slint)', format: 'rgba8unorm', alpha: 'opaque' },
  { name: 'rgba8unorm + premultiplied', format: 'rgba8unorm', alpha: 'premultiplied' },
];

for (const c of CASES) {
  test(c.name, async ({ page }) => {
    await page.goto(`${BASE}/rafprobe.html`);
    await page.evaluate(
      async ({ format, alpha }) => {
        const adapter = await navigator.gpu.requestAdapter();
        if (!adapter) throw new Error('没有 WebGPU adapter');
        const dev = await adapter.requestDevice();
        const canvas = document.createElement('canvas');
        canvas.width = 1600;
        canvas.height = 1125;
        canvas.style.width = '1280px';
        canvas.style.height = '900px';
        document.body.appendChild(canvas);
        const ctx = canvas.getContext('webgpu') as GPUCanvasContext | null;
        if (!ctx) throw new Error('拿不到 webgpu context');
        ctx.configure({
          device: dev,
          format: format as GPUTextureFormat,
          alphaMode: alpha as GPUCanvasAlphaMode,
        });
        (function loop() {
          const enc = dev.createCommandEncoder();
          const pass = enc.beginRenderPass({
            colorAttachments: [
              {
                view: ctx.getCurrentTexture().createView(),
                clearValue: { r: 0.1, g: 0.15, b: 0.3, a: 1 },
                loadOp: 'clear',
                storeOp: 'store',
              },
            ],
          });
          pass.end();
          dev.queue.submit([enc.finish()]);
          requestAnimationFrame(loop);
        })();
      },
      { format: c.format, alpha: c.alpha },
    );
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
      `${c.name}: rAF ${per('FireAnimationFrame')}/s,` +
        `DrawAndSwap ${per('Display::DrawAndSwap')}/s,` +
        `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms,>5ms ${subs.filter((x) => x > 5).length} 次`,
    );
  });
}
