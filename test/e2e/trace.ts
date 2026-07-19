// 录一段 Chrome trace 并算出帧的成本结构。
//
// 为什么不用页面里的 performance.now() 自测帧率:那只能看到"我这一帧的回调跑了多久",
// 看不到帧被谁卡住的。真正的答案在 GPU 进程里 —— 而 GPU 进程的事件是**全浏览器共享**的,
// 必须按本页的 renderer pid 过滤,否则别的标签页的开销会算到你头上(这个坑踩过两次,
// 见 docs/wasm/frame-rate.md 的方法论错误二)。
import type { Page } from '@playwright/test';

type TraceEvent = {
  pid: number;
  name?: string;
  ts?: number;
  dur?: number;
  ph?: string;
  args?: { data?: { renderer_pid?: number } };
};

// trace 里 ts/dur 都是可选的,但过滤之后就一定有。用类型守卫把这件事说给编译器听,
// 好过在每个取值处补一个 `!` —— 断言只是把检查关掉,守卫是把条件和类型绑在一起。
type Timed = TraceEvent & { ts: number };
type Timing = TraceEvent & { dur: number };

export type FrameStats = {
  /** 采样窗口,取本页首末 rAF 之间。分母必须从数据里取,不能拍脑袋。 */
  windowSec: number;
  rafFires: number;
  fps: number;
  rafGapMedianMs: number;
  beginFramePerSec: number;
  /** 主线程 RunTask 占墙钟的比例。 */
  mainThreadBusy: number;
  /** 本页在 GPU 进程里占墙钟的比例。判断资源吃紧看这个,不看单次中位数。 */
  gpuBusy: number;
  /** 每次 rAF 摊到的 GPU 进程时间。 */
  gpuMsPerFrame: number;
};

// -* 关掉默认集合,只留需要的:少一个数量级的事件量,解析快得多。
const CATEGORIES = [
  '-*',
  'devtools.timeline',
  'disabled-by-default-devtools.timeline',
  'disabled-by-default-devtools.timeline.frame',
  'toplevel',
  'gpu',
  'viz',
  'latency',
].join(',');

function median(xs: number[]): number {
  if (xs.length === 0) return Number.NaN;
  const s = [...xs].sort((a, b) => a - b);
  return s[s.length >> 1];
}

export async function recordFrames(
  page: Page,
  seconds: number,
): Promise<FrameStats> {
  const cdp = await page.context().newCDPSession(page);
  const events: TraceEvent[] = [];
  // Playwright 把 dataCollected 的 value 声明成 { [k: string]: string }[],与 trace 的
  // 真实形状对不上(ts/dur 是数字,args 是对象),只能自己断言。
  cdp.on('Tracing.dataCollected', (e) => {
    events.push(...(e.value as unknown as TraceEvent[]));
  });
  const done = new Promise<void>((resolve) =>
    cdp.once('Tracing.tracingComplete', () => resolve()),
  );

  await cdp.send('Tracing.start', {
    categories: CATEGORIES,
    transferMode: 'ReportEvents',
  });
  await page.waitForTimeout(seconds * 1000);
  await cdp.send('Tracing.end');
  await done;
  await cdp.detach();

  return analyze(events);
}

export function analyze(events: TraceEvent[]): FrameStats {
  // 本页的 renderer 进程:只有它会发 AnimationFrame / ProfileChunk。
  const byPid = new Map<number, number>();
  for (const e of events) {
    if (
      e.name === 'AnimationFrame' ||
      e.name === 'ProfileChunk'
    ) {
      byPid.set(e.pid, (byPid.get(e.pid) ?? 0) + 1);
    }
  }
  if (byPid.size === 0)
    throw new Error(
      'trace 里没有本页的 AnimationFrame,窗口可能没在前台',
    );
  const myPid = [...byPid.entries()].sort(
    (a, b) => b[1] - a[1],
  )[0][0];

  const mine = events.filter((e) => e.pid === myPid);
  const fires = mine
    .filter(
      (e): e is Timed =>
        e.name === 'FireAnimationFrame' &&
        (e.ph === 'X' || e.ph === 'B') &&
        e.ts !== undefined,
    )
    .sort((a, b) => a.ts - b.ts);
  if (fires.length < 2)
    throw new Error('trace 里几乎没有 rAF,页面可能没在画');

  const t0 = fires[0].ts;
  const t1 = fires[fires.length - 1].ts;
  const windowSec = (t1 - t0) / 1e6;
  const inWindow = (e: TraceEvent) =>
    e.ts !== undefined && e.ts >= t0 && e.ts <= t1;

  const gaps: number[] = [];
  for (let i = 1; i < fires.length; i++)
    gaps.push((fires[i].ts - fires[i - 1].ts) / 1000);
  const gapMedian = median(gaps);

  const mainBusyUs = mine
    .filter(
      (e): e is Timing =>
        e.name === 'RunTask' &&
        e.dur !== undefined &&
        inWindow(e),
    )
    .reduce((sum, e) => sum + e.dur, 0);

  const gpuUs = events
    .filter(
      (e): e is Timing =>
        e.name === 'GPUTask' &&
        e.dur !== undefined &&
        inWindow(e) &&
        e.args?.data?.renderer_pid === myPid,
    )
    .reduce((sum, e) => sum + e.dur, 0);

  const beginFrames = mine.filter(
    (e) => e.name === 'BeginFrame' && inWindow(e),
  ).length;

  return {
    windowSec,
    rafFires: fires.length,
    fps: 1000 / gapMedian,
    rafGapMedianMs: gapMedian,
    beginFramePerSec: beginFrames / windowSec,
    mainThreadBusy: mainBusyUs / 1e6 / windowSec,
    gpuBusy: gpuUs / 1e6 / windowSec,
    gpuMsPerFrame: gpuUs / 1000 / fires.length,
  };
}

export function format(
  label: string,
  s: FrameStats,
): string {
  return [
    `${label}:`,
    `  窗口 ${s.windowSec.toFixed(2)}s,rAF ${s.rafFires} 次`,
    `  帧率 ${s.fps.toFixed(1)}fps(间隔中位 ${s.rafGapMedianMs.toFixed(2)}ms),浏览器拍子 ${s.beginFramePerSec.toFixed(1)}/s`,
    `  主线程占用 ${(s.mainThreadBusy * 100).toFixed(0)}%,GPU 进程占用 ${(s.gpuBusy * 100).toFixed(0)}%`,
    `  GPU 每帧 ${s.gpuMsPerFrame.toFixed(2)}ms`,
  ].join('\n');
}
