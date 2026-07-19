// 探查:Slint 每帧对 canvas 上下文做了什么。
//
// 配置、尺寸、格式、device、渲染路径形状全部复刻过,裸画布都是 141/s 且提交不阻塞。
// 那差别只能在调用序列本身 —— 比如每帧重新 configure(等于重建 swapchain),
// 或者一帧里多次 getCurrentTexture。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

const CAPTURE = `
  window.__calls = { configure: 0, getCurrentTexture: 0, submit: 0, raf: 0 };
  const cfg = GPUCanvasContext.prototype.configure;
  GPUCanvasContext.prototype.configure = function (...a) {
    window.__calls.configure++;
    return cfg.apply(this, a);
  };
  const gct = GPUCanvasContext.prototype.getCurrentTexture;
  GPUCanvasContext.prototype.getCurrentTexture = function (...a) {
    window.__calls.getCurrentTexture++;
    return gct.apply(this, a);
  };
  const sub = GPUQueue.prototype.submit;
  GPUQueue.prototype.submit = function (...a) {
    window.__calls.submit++;
    return sub.apply(this, a);
  };
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => raf((t) => { window.__calls.raf++; return cb(t); });
`;

test('每帧的上下文调用次数', async ({ page }) => {
  await page.addInitScript(CAPTURE);
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);

  const before = await page.evaluate(
    () => ({ ...(window as unknown as { __calls: Record<string, number> }).__calls }),
  );
  await page.waitForTimeout(5000);
  const after = await page.evaluate(
    () => ({ ...(window as unknown as { __calls: Record<string, number> }).__calls }),
  );

  const frames = after.raf - before.raf;
  console.log(`5 秒内 ${frames} 帧`);
  for (const k of ['configure', 'getCurrentTexture', 'submit'] as const) {
    const n = after[k] - before[k];
    console.log(`  ${k.padEnd(20)} ${n} 次,${(n / Math.max(1, frames)).toFixed(2)} 次/帧`);
  }
});
