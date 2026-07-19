// 探查:Slint 的 getCurrentTexture / submit 到底在不在 rAF 回调那个任务里。
//
// 浏览器是在**任务结束时**把 WebGPU 画布呈现出去的。若取图像和提交落在 rAF 之外的
// 另一个任务里,这一帧就赶不上当拍 —— 而这是 JS 复刻件结构上做不出来的差异,
// 也是排除掉配置、device、路径形状、调用序列、主线程占用之后仅剩的一条。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

const CAPTURE = `
  window.__obs = [];
  let cur = null;
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => raf((t) => {
    cur = { start: performance.now(), end: null, gct: [], submit: [] };
    window.__obs.push(cur);
    if (window.__obs.length > 400) window.__obs.shift();
    const r = cb(t);
    cur.end = performance.now();
    const done = cur;
    cur = null;
    // 任务结束后才跑的宏任务:落在它之后的调用一定不在这一帧的任务里。
    setTimeout(() => { done.taskEnded = performance.now(); }, 0);
    return r;
  });
  const gct = GPUCanvasContext.prototype.getCurrentTexture;
  GPUCanvasContext.prototype.getCurrentTexture = function (...a) {
    (cur ? cur.gct : (window.__orphanGct = (window.__orphanGct ?? 0) + 1, [])).push?.(performance.now());
    return gct.apply(this, a);
  };
  const sub = GPUQueue.prototype.submit;
  GPUQueue.prototype.submit = function (...a) {
    (cur ? cur.submit : (window.__orphanSubmit = (window.__orphanSubmit ?? 0) + 1, [])).push?.(performance.now());
    return sub.apply(this, a);
  };
`;

test('取图像与提交在不在 rAF 任务里', async ({ page }) => {
  await page.addInitScript(CAPTURE);
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(14_000);

  const r = await page.evaluate(() => {
    const w = window as unknown as {
      __obs: { start: number; end: number; gct: number[]; submit: number[] }[];
      __orphanGct?: number;
      __orphanSubmit?: number;
    };
    const obs = w.__obs.slice(-200);
    const dur = obs.map((o) => o.end - o.start).sort((a, b) => a - b);
    const gap = obs.slice(1).map((o, i) => o.start - obs[i].start).sort((a, b) => a - b);
    const gctIn = obs.filter((o) => o.gct.length > 0).length;
    const subIn = obs.filter((o) => o.submit.length > 0).length;
    return {
      frames: obs.length,
      回调时长中位: dur[dur.length >> 1],
      回调时长p90: dur[Math.floor(dur.length * 0.9)],
      帧间隔中位: gap[gap.length >> 1],
      有取图像的帧: gctIn,
      有提交的帧: subIn,
      rAF之外的取图像: w.__orphanGct ?? 0,
      rAF之外的提交: w.__orphanSubmit ?? 0,
    };
  });
  console.log(JSON.stringify(r, null, 2));
});
