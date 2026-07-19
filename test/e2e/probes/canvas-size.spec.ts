// 复核:视口扫描时 Slint 的画布到底有没有跟着缩。
// index.html 里有 min-width,画布可能纹丝不动 —— 那样"与像素无关"的结论就是假的。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const SIZES = [
  { width: 1280, height: 900 },
  { width: 640, height: 480 },
  { width: 320, height: 240 },
  { width: 200, height: 150 },
];

test('视口缩小时画布跟不跟着缩', async ({ page }) => {
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);
  for (const s of SIZES) {
    await page.setViewportSize(s);
    await page.waitForTimeout(2000);
    const info = await page.evaluate(() => {
      const c = document.querySelector('canvas');
      if (!c) return null;
      return {
        backing: `${c.width}x${c.height}`,
        css: `${Math.round(c.getBoundingClientRect().width)}x${Math.round(c.getBoundingClientRect().height)}`,
        dpr: window.devicePixelRatio,
        pixels: c.width * c.height,
      };
    });
    console.log(`视口 ${s.width}x${s.height} → 画布 ${JSON.stringify(info)}`);
  }
});
