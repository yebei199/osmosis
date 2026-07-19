// 探查:GPU 进程里最长的那几个跨度,内部到底套着什么。
// 已知每帧只有 13 条 femtovg 命令、626 个顶点,却要花 17.9ms —— 找出等在哪一行。
import { test } from '@playwright/test';
import { WEB_PORT } from '../playwright.config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const QUERY = process.env.PROBE_QUERY ?? 'tab=2&bevy=off';

type Ev = {
  pid: number;
  tid: number;
  name?: string;
  cat?: string;
  ts?: number;
  dur?: number;
  args?: { data?: { renderer_pid?: number } };
};
type Span = Ev & { name: string; ts: number; dur: number };
const isSpan = (e: Ev): e is Span =>
  e.name !== undefined && e.ts !== undefined && e.dur !== undefined;

test('最长跨度里面是什么', async ({ page }) => {
  await page.goto(`${BASE}/?${QUERY}`);
  await page.waitForTimeout(12_000);

  const cdp = await page.context().newCDPSession(page);
  const events: Ev[] = [];
  cdp.on('Tracing.dataCollected', (e) => {
    events.push(...(e.value as unknown as Ev[]));
  });
  const done = new Promise<void>((r) =>
    cdp.once('Tracing.tracingComplete', () => r()),
  );
  // 这里刻意把类别开到最宽:要找的是一个没被现有类别覆盖的等待点。
  await cdp.send('Tracing.start', {
    categories: [
      'toplevel',
      'toplevel.flow',
      'gpu',
      'viz',
      'sequence_manager',
      'gpu.service',
      'disabled-by-default-gpu.service',
      'disabled-by-default-gpu.device',
      'disabled-by-default-gpu.dawn',
      'disabled-by-default-toplevel.flow',
      'devtools.timeline',
      'disabled-by-default-devtools.timeline',
    ].join(','),
    transferMode: 'ReportEvents',
  });
  await page.waitForTimeout(3000);
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
    .filter((e) => e.name === 'GPUTask' && e.args?.data?.renderer_pid === myPid)
    .sort((a, b) => b.dur - a.dur);
  console.log(`本页 GPUTask ${gpuTasks.length} 个,最长 ${(gpuTasks[0].dur / 1000).toFixed(2)}ms`);

  const gpuPid = gpuTasks[0].pid;
  const gpuTid = gpuTasks[0].tid;
  const all = events
    .filter(isSpan)
    .filter((e) => e.pid === gpuPid && e.tid === gpuTid)
    .sort((a, b) => a.ts - b.ts || b.dur - a.dur);

  for (const task of gpuTasks.slice(0, 3)) {
    console.log(`\n=== GPUTask ${(task.dur / 1000).toFixed(2)}ms 内部 ===`);
    const inner = all.filter(
      (e) => e.ts >= task.ts && e.ts + e.dur <= task.ts + task.dur,
    );
    // 缩进按嵌套深度,并标出与上一条之间的空档 —— 空档就是没有任何事件在跑的等待。
    const stack: number[] = [];
    let prevEnd = task.ts;
    for (const e of inner.slice(0, 40)) {
      while (stack.length > 0 && stack[stack.length - 1] <= e.ts) stack.pop();
      const gap = (e.ts - prevEnd) / 1000;
      const indent = '  '.repeat(stack.length);
      const gapMark = gap > 0.3 ? `  ← 空档 ${gap.toFixed(2)}ms` : '';
      console.log(`  ${indent}${e.name} (${(e.dur / 1000).toFixed(2)}ms)${gapMark}`);
      prevEnd = Math.max(prevEnd, e.ts + e.dur);
      stack.push(e.ts + e.dur);
    }
  }
});
