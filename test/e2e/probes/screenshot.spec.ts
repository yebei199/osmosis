// 画面正确性:动态偏移写错会渲染出垃圾,而帧率照样好看 —— 只看数字发现不了。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const QUERY = process.env.PROBE_QUERY ?? 'tab=2';

test('截一张真实画面', async ({ page }, testInfo) => {
  const errors: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error') errors.push(m.text());
  });
  await page.goto(`${BASE}/?${QUERY}`);
  await page.waitForTimeout(14_000);
  const out = testInfo.outputPath(`shot-${QUERY.replace(/[^a-z0-9]/gi, '_')}.png`);
  await page.screenshot({ path: out });
  console.log(`截图: ${out}`);
  if (errors.length > 0) console.log(`控制台错误:\n${errors.slice(0, 5).join('\n')}`);
});
