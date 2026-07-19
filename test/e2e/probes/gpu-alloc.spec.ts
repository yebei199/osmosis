// 探查:femtovg 的 wgpu 后端每帧真正创建了多少 GPU 对象。
//
// 源码显示每个 draw 在 bind group 缓存未命中时会建 6 个对象(uniform buffer、2 个 sampler、
// 2 个 texture view、1 个 bind group),而那个"缓存"只是单槽的上一次值比较。
// 但测试用的矩形颜色全相同,单槽缓存本该命中 —— 先数清楚再决定改哪里。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

const CAPTURE = `
  window.__gpu = { buffer: 0, bindGroup: 0, sampler: 0, view: 0, encoder: 0, raf: 0 };
  const d = GPUDevice.prototype;
  for (const [m, k] of [['createBuffer','buffer'],['createBindGroup','bindGroup'],
                        ['createSampler','sampler'],['createCommandEncoder','encoder']]) {
    const orig = d[m];
    d[m] = function (...a) { window.__gpu[k]++; return orig.apply(this, a); };
  }
  const cv = GPUTexture.prototype.createView;
  GPUTexture.prototype.createView = function (...a) { window.__gpu.view++; return cv.apply(this, a); };
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => raf((t) => { window.__gpu.raf++; return cb(t); });
`;

for (const rects of [0, 200]) {
  test(`?rects=${rects} 每帧建了多少 GPU 对象`, async ({ page }) => {
    await page.addInitScript(CAPTURE);
    await page.goto(`${BASE}/?rects=${rects}`);
    await page.waitForTimeout(10_000);
    const a = await page.evaluate(() => ({ ...(window as unknown as { __gpu: Record<string, number> }).__gpu }));
    await page.waitForTimeout(4000);
    const b = await page.evaluate(() => ({ ...(window as unknown as { __gpu: Record<string, number> }).__gpu }));
    const frames = b.raf - a.raf;
    console.log(`?rects=${rects} —— ${frames} 帧`);
    for (const k of ['buffer', 'bindGroup', 'sampler', 'view', 'encoder'] as const) {
      const n = b[k] - a[k];
      console.log(`  ${k.padEnd(12)} ${String(n).padStart(7)} 次,${(n / Math.max(1, frames)).toFixed(1)} 次/帧`);
    }
  });
}
