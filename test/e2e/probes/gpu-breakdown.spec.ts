// 探查:把 3D 页每帧的 GPU 时间按事件名拆开,并数出每帧建了几条渲染管线。
//
// 定位工具,不是回归测试,所以不断言,只出数。留着的理由:每帧管线数是"修好了没有"的
// 直接判据 —— 帧率是症状,管线数是原因。
//
// PROBE_TAB=0 换成不驱动 bevy 的页面做对照。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const TAB = process.env.PROBE_TAB ?? '2';

type Ev = {
  pid: number;
  tid: number;
  name?: string;
  ts?: number;
  dur?: number;
  args?: { data?: { renderer_pid?: number } };
};

/** trace 里 ts/dur 是可选的,过滤之后必然存在 —— 用守卫把这件事说给编译器听。 */
type Span = Ev & { name: string; ts: number; dur: number };

const isSpan = (e: Ev): e is Span =>
  e.name !== undefined && e.ts !== undefined && e.dur !== undefined;

const CATEGORIES = [
  '-*',
  'toplevel',
  'gpu',
  'viz',
  'gpu.service',
  'disabled-by-default-gpu.service',
  'disabled-by-default-gpu.device',
  // 管线创建、Queue::Submit 这些 Dawn 内部事件只在这个类别里。
  'disabled-by-default-gpu.dawn',
  'devtools.timeline',
  'disabled-by-default-devtools.timeline',
].join(',');

test('拆开 GPU 进程里的每帧开销', async ({ page }) => {
  await page.goto(`${BASE}/?tab=${TAB}`);
  console.log(`=== tab=${TAB} ===`);
  // wasm 34MB,还要建 device、编管线,等它进稳态再采。
  await page.waitForTimeout(12_000);

  const cdp = await page.context().newCDPSession(page);
  const events: Ev[] = [];
  cdp.on('Tracing.dataCollected', (e) => {
    events.push(...(e.value as unknown as Ev[]));
  });
  const done = new Promise<void>((r) =>
    cdp.once('Tracing.tracingComplete', () => r()),
  );
  await cdp.send('Tracing.start', {
    categories: CATEGORIES,
    transferMode: 'ReportEvents',
  });
  await page.waitForTimeout(5000);
  await cdp.send('Tracing.end');
  await done;

  const byPid = new Map<number, number>();
  for (const e of events) {
    if (e.name === 'AnimationFrame' || e.name === 'ProfileChunk') {
      byPid.set(e.pid, (byPid.get(e.pid) ?? 0) + 1);
    }
  }
  const myPid = [...byPid.entries()].sort((a, b) => b[1] - a[1])[0][0];

  const gpuTasks = events
    .filter(isSpan)
    .filter(
      (e) => e.name === 'GPUTask' && e.args?.data?.renderer_pid === myPid,
    );
  if (gpuTasks.length === 0) {
    throw new Error('没采到本页的 GPUTask,窗口可能没在前台');
  }
  const gpuPid = gpuTasks[0].pid;
  const gpuTid = gpuTasks[0].tid;
  const totalMs = gpuTasks.reduce((s, e) => s + e.dur, 0) / 1000;
  console.log(`本页 renderer pid=${myPid},GPU 进程 pid=${gpuPid}`);
  console.log(`GPUTask ${gpuTasks.length} 个,合计 ${totalMs.toFixed(0)}ms`);

  // 帧数从 rAF 数,不要从 GPUTask 数推 —— 每帧几个 GPUTask 本身就是要测的量。
  const frames = Math.max(
    1,
    events.filter(
      (e) =>
        e.pid === myPid &&
        e.name === 'FireAnimationFrame' &&
        e.ts !== undefined,
    ).length,
  );
  console.log(`rAF ${frames} 次`);

  // 这些区间是嵌套的,总耗时互相包含 —— 直接按总耗时排序,最前面永远是最外层的壳。
  // 算独占时间:自己的 dur 减掉直接落在自己区间内的子区间。同一条线程上按栈还原父子关系。
  const spans = gpuTasks
    .map((e) => [e.ts, e.ts + e.dur] as const)
    .sort((a, b) => a[0] - b[0]);
  const insideFrame = (ts: number) => spans.some(([a, b]) => ts >= a && ts <= b);

  const onThread = events
    .filter(isSpan)
    .filter((e) => e.pid === gpuPid && e.tid === gpuTid && insideFrame(e.ts))
    .sort((a, b) => a.ts - b.ts || b.dur - a.dur);

  const stat = new Map<string, { n: number; self: number; total: number }>();
  const get = (name: string) => {
    const cur = stat.get(name) ?? { n: 0, self: 0, total: 0 };
    stat.set(name, cur);
    return cur;
  };
  const stack: { end: number; name: string }[] = [];
  for (const e of onThread) {
    while (stack.length > 0 && stack[stack.length - 1].end <= e.ts) stack.pop();
    const parent = stack[stack.length - 1];
    if (parent !== undefined) get(parent.name).self -= e.dur;
    const cur = get(e.name);
    cur.n += 1;
    cur.self += e.dur;
    cur.total += e.dur;
    stack.push({ end: e.ts + e.dur, name: e.name });
  }

  console.log('\nGPU 进程内、我们帧区间里的事件(按独占时间):');
  for (const [name, v] of [...stat]
    .sort((a, b) => b[1].self - a[1].self)
    .slice(0, 15)) {
    console.log(
      `  ${name.padEnd(44)} n=${String(v.n).padStart(5)}` +
        `  独占 ${(v.self / 1000).toFixed(1)}ms  总 ${(v.total / 1000).toFixed(1)}ms` +
        `  ${(v.self / 1000 / frames).toFixed(2)}ms/帧`,
    );
  }

  console.log('\n每帧调用次数:');
  for (const [name, v] of [...stat]
    .sort((a, b) => b[1].n - a[1].n)
    .slice(0, 12)) {
    console.log(`  ${name.padEnd(44)} ${(v.n / frames).toFixed(1)} 次/帧`);
  }
});
