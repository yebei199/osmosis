// 探查:把 bevy 的开销与 Slint 自己的分开。
// ?bevy=off 跳过驱动渲染器但照常请求重绘,界面以同样的节奏画,只是不含 bevy 那份工作。
import { test } from '@playwright/test';
import { WEB_PORT } from '../playwright.config';
import { format, recordFrames } from '../trace';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

for (const q of ['tab=2', 'tab=2&bevy=off', 'tab=0']) {
  test(`?${q}`, async ({ page }) => {
    await page.goto(`${BASE}/?${q}`);
    await page.waitForTimeout(12_000);
    console.log(format(`?${q}`, await recordFrames(page, 5)));
  });
}
