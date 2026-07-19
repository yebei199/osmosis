// 探查:在应用页里再挂一条裸 WebGPU 呈现循环,看它是否同样被卡。
//
// 卡住 = 问题在页面/合成器这一层(整页被限到 60Hz);
// 畅通 = 问题在 Slint 那张 surface 上(同一页里两张画布待遇不同)。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = {
  pid: number;
  tid: number;
  name?: string;
  ts?: number;
  dur?: number;
  args?: { data?: { renderer_pid?: number } };
};
type Span = Ev & { name: string; ts: number; dur: number };
const isSpan = (e: Ev): e is Span =>
  e.name !== undefined && e.ts !== undefined && e.dur !== undefined;

test('应用页里再挂一条裸 WebGPU 循环', async ({ page }) => {
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);

  // 注入之前先量一遍,作为同一页面的基线。
  const before = await page.evaluate(
    () =>
      new Promise<number>((resolve) => {
        const ts: number[] = [];
        const t0 = performance.now();
        (function loop(t: number) {
          ts.push(t);
          if (performance.now() - t0 < 2000) requestAnimationFrame(loop);
          else {
            const d = ts.slice(1).map((x, i) => x - ts[i]).sort((a, b) => a - b);
            resolve(1000 / d[d.length >> 1]);
          }
        })(performance.now());
      }),
  );
  console.log(`注入前,应用页 rAF ${before.toFixed(1)}fps`);

  await page.evaluate(async () => {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) throw new Error('没有 WebGPU adapter');
    const dev = await adapter.requestDevice();
    const canvas = document.createElement('canvas');
    canvas.width = 400;
    canvas.height = 300;
    canvas.style.position = 'fixed';
    canvas.style.right = '0';
    canvas.style.bottom = '0';
    canvas.style.width = '200px';
    canvas.style.height = '150px';
    document.body.appendChild(canvas);
    const ctx = canvas.getContext('webgpu') as GPUCanvasContext | null;
    if (!ctx) throw new Error('拿不到 webgpu context');
    ctx.configure({ device: dev, format: 'rgba8unorm', alphaMode: 'opaque' });
    (function loop() {
      const enc = dev.createCommandEncoder({ label: 'PROBE-bare-canvas' });
      const pass = enc.beginRenderPass({
        colorAttachments: [
          {
            view: ctx.getCurrentTexture().createView(),
            clearValue: { r: 0.6, g: 0.1, b: 0.1, a: 1 },
            loadOp: 'clear',
            storeOp: 'store',
          },
        ],
      });
      pass.end();
      dev.queue.submit([enc.finish()]);
      requestAnimationFrame(loop);
    })();
  });
  await page.waitForTimeout(3000);

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

  const rafs = events.filter((e) => e.name === 'FireAnimationFrame').length / 4;
  const ticks = events.filter((e) => e.name === 'DelayBasedBeginFrameSource::OnTimerTick').length / 4;
  const swaps = events.filter((e) => e.name === 'Display::DrawAndSwap').length / 4;
  console.log(`注入后:rAF ${rafs.toFixed(1)}/s,拍子 ${ticks.toFixed(1)}/s,DrawAndSwap ${swaps.toFixed(1)}/s`);

  const submits = events.filter(isSpan).filter((e) => e.name === 'Queue::Submit');
  const ms = submits.map((e) => e.dur / 1000).sort((a, b) => a - b);
  const long = ms.filter((x) => x > 5).length;
  console.log(
    `Queue::Submit ${submits.length} 次(${(submits.length / 4).toFixed(1)}/s):` +
      ` 中位 ${ms[ms.length >> 1]?.toFixed(2)}ms,>5ms 的有 ${long} 次`,
  );
});
