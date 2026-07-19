// 探查:GPU 进程那 90% 占用是在干活,还是在等?
// 把视口从大扫到极小,像素量掉两个数量级。占用率跟着掉 = 真在光栅化;不动 = 在等。
import { test } from '@playwright/test';
import { WEB_PORT } from './playwright.config';
import { format, recordFrames } from './trace';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const SIZES: { width: number; height: number }[] = [
  { width: 1280, height: 900 },
  { width: 640, height: 480 },
  { width: 320, height: 240 },
  { width: 200, height: 150 },
];

test('视口大小扫描', async ({ page }) => {
  await page.goto(`${BASE}/?tab=2`);
  await page.waitForTimeout(12_000);
  for (const s of SIZES) {
    await page.setViewportSize(s);
    await page.waitForTimeout(3000);
    const stats = await recordFrames(page, 4);
    console.log(format(`${s.width}x${s.height}`, stats));
  }
});
