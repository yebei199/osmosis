// 读打了桩的 femtovg 从浏览器控制台吐出来的日志(需要 Cargo.toml 里挂上本地副本,
// 办法见 docs/wasm/frame-rate.md 第七节)。
import { test } from '@playwright/test';
import { WEB_PORT } from '../playwright.config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const QUERY = process.env.PROBE_QUERY ?? 'tab=2';

test('femtovg 打桩日志', async ({ page }) => {
  const lines: string[] = [];
  page.on('console', (m) => {
    const t = m.text();
    if (t.includes('PROBE')) lines.push(t);
  });
  await page.goto(`${BASE}/?${QUERY}`);
  await page.waitForTimeout(15_000);

  console.log(`?${QUERY} —— 共 ${lines.length} 行`);
  console.log('--- 稳态最后 12 行 ---');
  for (const l of lines.slice(-12)) console.log(l);
});
