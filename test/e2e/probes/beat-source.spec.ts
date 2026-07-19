// 探查:应用页的拍子是 60/s,对照页是 144/s。这是 Chrome 给的拍子本身不同,
// 还是我们跟不上被降频的结果?看两页各自的 BeginFrame 来源与频率。
import { test } from '@playwright/test';
import { WEB_PORT } from '../playwright.config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { pid: number; name?: string; ts?: number; dur?: number };

const CATEGORIES = ['toplevel', 'gpu', 'viz', 'devtools.timeline', 'disabled-by-default-devtools.timeline'].join(',');

async function beats(page: import('@playwright/test').Page, label: string) {
  const cdp = await page.context().newCDPSession(page);
  const events: Ev[] = [];
  cdp.on('Tracing.dataCollected', (e) => {
    events.push(...(e.value as unknown as Ev[]));
  });
  const done = new Promise<void>((r) =>
    cdp.once('Tracing.tracingComplete', () => r()),
  );
  await cdp.send('Tracing.start', { categories: CATEGORIES, transferMode: 'ReportEvents' });
  await page.waitForTimeout(4000);
  await cdp.send('Tracing.end');
  await done;
  await cdp.detach();

  const count = (n: string) => events.filter((e) => e.name === n).length / 4;
  console.log(`\n${label}`);
  for (const n of [
    'FireAnimationFrame',
    'BeginFrame',
    'BeginMainThreadFrame',
    'DrawFrame',
    'DelayBasedBeginFrameSource::OnTimerTick',
    'DisplayScheduler::OnBeginFrameDeadline',
    'Display::DrawAndSwap',
  ]) {
    console.log(`  ${n.padEnd(42)} ${count(n).toFixed(1)}/s`);
  }
}

test('两页的拍子来源', async ({ page }) => {
  await page.goto(`${BASE}/rafprobe.html`);
  await page.evaluate(async () => {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) throw new Error('没有 WebGPU adapter');
    const dev = await adapter.requestDevice();
    const canvas = document.createElement('canvas');
    canvas.width = 1605;
    canvas.height = 1984;
    canvas.style.width = '1284px';
    canvas.style.height = '1587px';
    document.body.appendChild(canvas);
    const ctx = canvas.getContext('webgpu') as GPUCanvasContext | null;
    if (!ctx) throw new Error('拿不到 webgpu context');
    ctx.configure({ device: dev, format: 'rgba8unorm', alphaMode: 'opaque' });
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
  });
  await page.waitForTimeout(2000);
  await beats(page, '对照页 rafprobe');

  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);
  await beats(page, '应用页 ?tab=2&bevy=off');
});
