// 探查:rAF 回调在一拍里的相位。
//
// rAF 的时间戳 t 是浏览器给这一帧定的起点,回调进入时的 performance.now() 减掉它,
// 就是"这一帧的 JS 比帧起点晚了多久"。晚得多说明主线程在被别的事情占着,当拍来不及提交,
// 于是只能落到下一拍 —— 与实测的"隔一拍"(13.9ms = 2×6.94ms)对得上。
//
// 同时量回调结束到下一帧起点之间的空档:空档大而回调短,说明卡的不是我们的代码。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const QUERY = process.env.PROBE_QUERY ?? 'tab=2&bevy=off';

const CAPTURE = `
  window.__phase = [];
  const raf = window.requestAnimationFrame.bind(window);
  window.requestAnimationFrame = (cb) => raf((t) => {
    const enter = performance.now();
    const r = cb(t);
    window.__phase.push({ t, enter, exit: performance.now() });
    if (window.__phase.length > 400) window.__phase.shift();
    return r;
  });
`;

function stats(xs: number[]) {
  const s = [...xs].sort((a, b) => a - b);
  const q = (p: number) => s[Math.floor(s.length * p)];
  return `中位 ${q(0.5)?.toFixed(2)}ms  p10 ${q(0.1)?.toFixed(2)}  p90 ${q(0.9)?.toFixed(2)}`;
}

test('rAF 回调的相位', async ({ page }) => {
  await page.addInitScript(CAPTURE);
  await page.goto(`${BASE}/?${QUERY}`);
  await page.waitForTimeout(14_000);
  const rows = await page.evaluate(
    () =>
      (window as unknown as { __phase: { t: number; enter: number; exit: number }[] })
        .__phase.slice(-300),
  );

  const lateness = rows.map((r) => r.enter - r.t);
  const work = rows.map((r) => r.exit - r.enter);
  const beat = rows.slice(1).map((r, i) => r.t - rows[i].t);
  const idle = rows.slice(1).map((r, i) => r.t - rows[i].exit);

  console.log(`?${QUERY} —— ${rows.length} 帧`);
  console.log(`  帧起点间隔   ${stats(beat)}`);
  console.log(`  起点→进回调  ${stats(lateness)}`);
  console.log(`  回调耗时     ${stats(work)}`);
  console.log(`  回调结束→下一帧起点  ${stats(idle)}`);
});
