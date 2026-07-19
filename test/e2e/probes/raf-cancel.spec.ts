// 探查:winit 的 rAF 排队里有没有「取消 + 重排」。
//
// winit 的 AnimationFrameHandler::request() 会先 cancelAnimationFrame 掉已排队的那次
// 再重新 request。若这个动作发生在 rAF 回调**之外**的任务里(ResizeObserver、定时器),
// 已经排好的那一帧就会被推掉一拍 —— 正好是对折。
//
// 其余变量已全部排除:画布配置、device、渲染路径形状、每帧调用序列、主线程占用、
// 宿主页面结构,逐项复刻后裸循环都是 140/s。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

const CAPTURE = `
  window.__raf = { req: 0, cancel: 0, cancelInside: 0, cancelOutside: 0, reqOutside: 0, gaps: [] };
  let inside = false;
  let last = 0;
  const req = window.requestAnimationFrame.bind(window);
  const cancel = window.cancelAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => {
    window.__raf.req++;
    if (!inside) window.__raf.reqOutside++;
    return req((t) => {
      if (last) window.__raf.gaps.push(t - last);
      if (window.__raf.gaps.length > 300) window.__raf.gaps.shift();
      last = t;
      inside = true;
      try { return cb(t); } finally { inside = false; }
    });
  };
  window.cancelAnimationFrame = (h) => {
    window.__raf.cancel++;
    if (inside) window.__raf.cancelInside++; else window.__raf.cancelOutside++;
    return cancel(h);
  };
`;

test('rAF 的排队与取消', async ({ page }) => {
  await page.addInitScript(CAPTURE);
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(14_000);
  const r = await page.evaluate(() => {
    const w = window as unknown as { __raf: { gaps: number[] } & Record<string, number> };
    const g = [...w.__raf.gaps].sort((a, b) => a - b);
    return {
      请求: w.__raf.req,
      取消: w.__raf.cancel,
      回调内取消: w.__raf.cancelInside,
      回调外取消: w.__raf.cancelOutside,
      回调外请求: w.__raf.reqOutside,
      帧间隔中位: g[g.length >> 1],
    };
  });
  console.log(JSON.stringify(r, null, 2));
});
