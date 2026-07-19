// 最小复现:纯 Slint + femtovg-wgpu,无 3D、无 render3d,只有一个持续运行的动画。
// 需要 `just web-dev repro` 的产物。
//
// 跑满 → 问题是 3D 链路引入的;跑不满 → 是 Slint 自己,这份产物就是给上游的样例。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';
import { format, recordFrames } from '../trace';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

type Ev = { name?: string; ts?: number; dur?: number };

const QUERY = process.env.PROBE_QUERY ?? '';

test('最小复现的帧率', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error') errors.push(m.text());
  });
  const marks: string[] = [];
  page.on('console', (m) => {
    if (m.text().includes('PROBE repro build ready')) marks.push(m.text());
  });
  await page.goto(`${BASE}/?${QUERY}`);
  console.log(`?${QUERY}`);
  await page.waitForTimeout(12_000);
  if (errors.length > 0) console.log(`控制台错误:\n${errors.slice(0, 5).join('\n')}`);
  // dist/web 里是别的 feature 编出来的产物时,?rects= 不存在、页面也不是复现页,
  // 量出来的数看着像结论,其实什么都不是。
  if (marks.length === 0) {
    throw new Error('页面没报 repro 标记 —— dist/web 里不是 repro 产物,先重新构建');
  }

  // 焦点被别的窗口抢走时 rAF 会被压到 1Hz,而探针没有 frame-rate.spec 那样的哨兵 ——
  // 不检查的话只会安静地给出一个偏低的数字,据此下的结论就是错的。
  const focused = await page.evaluate(() => ({
    hasFocus: document.hasFocus(),
    visibility: document.visibilityState,
  }));
  if (!focused.hasFocus || focused.visibility !== 'visible') {
    throw new Error(
      `窗口不在前台(hasFocus=${focused.hasFocus} visibility=${focused.visibility}),量到的数不能用`,
    );
  }

  // 再确认画面真的在动 —— 静止的页面不重绘,量到的会是"1fps"的假象。
  const moving = await page.evaluate(
    () =>
      new Promise<number>((resolve) => {
        let n = 0;
        const t0 = performance.now();
        (function loop() {
          n++;
          if (performance.now() - t0 < 1500) requestAnimationFrame(loop);
          else resolve(n / 1.5);
        })();
      }),
  );
  console.log(`rAF 自测 ${moving.toFixed(1)}/s`);

  console.log(format('最小复现', await recordFrames(page, 5)));

  const cdp = await page.context().newCDPSession(page);
  const events: Ev[] = [];
  cdp.on('Tracing.dataCollected', (e) => {
    events.push(...(e.value as unknown as Ev[]));
  });
  const done = new Promise<void>((r) => cdp.once('Tracing.tracingComplete', () => r()));
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
    `DrawAndSwap ${per('Display::DrawAndSwap')}/s  ` +
      `Submit 中位 ${subs[subs.length >> 1]?.toFixed(2)}ms  >5ms ${subs.filter((x) => x > 5).length} 次`,
  );
});
