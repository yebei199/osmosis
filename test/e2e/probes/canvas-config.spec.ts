// 核对:线上那张画布的真实配置,逐项对着复刻件比。
// 复刻实验一路都是"没差别",那就得回头确认复刻的是不是真的一样。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

test('线上画布的真实配置', async ({ page }) => {
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);
  const info = await page.evaluate(() => {
    const c = document.querySelector('canvas');
    if (!c) return null;
    const ctx = c.getContext('webgpu') as GPUCanvasContext | null;
    const cfg = ctx?.getConfiguration?.() as
      | (GPUCanvasConfiguration & { usage?: number })
      | undefined;
    const cs = getComputedStyle(c);
    return {
      backing: `${c.width}x${c.height}`,
      css: `${cs.width}x${cs.height}`,
      dpr: window.devicePixelRatio,
      format: cfg?.format,
      alphaMode: cfg?.alphaMode,
      usage: cfg?.usage,
      colorSpace: (cfg as { colorSpace?: string } | undefined)?.colorSpace,
      toneMapping: JSON.stringify(
        (cfg as { toneMapping?: unknown } | undefined)?.toneMapping,
      ),
      attrs: [...c.attributes].map((a) => `${a.name}=${a.value}`).join(' '),
      parent: c.parentElement?.tagName,
      siblings: document.body.children.length,
      position: cs.position,
      transform: cs.transform,
      zIndex: cs.zIndex,
      opacity: cs.opacity,
      mixBlendMode: cs.mixBlendMode,
      filter: cs.filter,
      bodyBg: getComputedStyle(document.body).backgroundColor,
    };
  });
  console.log(JSON.stringify(info, null, 2));
});
