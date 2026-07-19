// 探查:把 Slint 渲染路径的形状逐项复刻到裸画布上,找出哪一项让提交开始阻塞。
//
// 已排除:尺寸、格式、alphaMode(见 format-swap)。剩下三处形状差异:
//   1. 离屏渲染再拷贝到画布(Slint 有 render_to_texture 管线和 TextureCopy)
//   2. 每帧多次提交(Slint 约 6 次)
//   3. 很早就取 swapchain 图像,取完还做很多事才结束
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };
type Shape = { offscreen: boolean; submits: number; acquireEarly: boolean };

const CASES: { name: string; shape: Shape }[] = [
  { name: '基线:直接画到画布', shape: { offscreen: false, submits: 1, acquireEarly: false } },
  { name: '离屏渲染再拷贝', shape: { offscreen: true, submits: 1, acquireEarly: false } },
  { name: '每帧 6 次提交', shape: { offscreen: false, submits: 6, acquireEarly: false } },
  { name: '提前取图像 + 6 次提交', shape: { offscreen: false, submits: 6, acquireEarly: true } },
  { name: '三者齐全(=Slint 形状)', shape: { offscreen: true, submits: 6, acquireEarly: true } },
];

for (const c of CASES) {
  test(c.name, async ({ page }) => {
    await page.goto(`${BASE}/rafprobe.html`);
    await page.evaluate(async (shape: Shape) => {
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
      // 用字面量而非 GPUTextureUsage:那是运行期的全局量,TypeScript 的 lib.dom
      // 只声明了类型没声明值。COPY_SRC=1,COPY_DST=2,RENDER_ATTACHMENT=16。
      ctx.configure({
        device: dev,
        format: 'rgba8unorm',
        alphaMode: 'opaque',
        usage: 16 | 2,
      });
      const offscreen = dev.createTexture({
        size: [1600, 1125],
        format: 'rgba8unorm',
        usage: 16 | 1,
      });

      const clearInto = (enc: GPUCommandEncoder, view: GPUTextureView) => {
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
      };

      (function loop() {
        // 提前取图像:取完之后还要走完全部提交才结束这一帧。
        const early = shape.acquireEarly ? ctx.getCurrentTexture() : null;
        for (let i = 0; i < shape.submits; i++) {
          const enc = dev.createCommandEncoder();
          if (i === 0) {
            if (shape.offscreen) {
              clearInto(enc, offscreen.createView());
              const target = early ?? ctx.getCurrentTexture();
              enc.copyTextureToTexture(
                { texture: offscreen },
                { texture: target },
                [1600, 1125],
              );
            } else {
              clearInto(enc, (early ?? ctx.getCurrentTexture()).createView());
            }
          }
          dev.queue.submit([enc.finish()]);
        }
        requestAnimationFrame(loop);
      })();
    }, c.shape);
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
      `${c.name.padEnd(24)} rAF ${per('FireAnimationFrame')}/s  ` +
        `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
        `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
    );
  });
}
