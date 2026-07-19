// 探查:应用页和对照页拿到的是不是同一块 GPU。
//
// 若 wgpu 挑的 adapter 与浏览器合成器用的不是同一块,每帧就要跨卡拷贝 —— 那正是一个
// 藏在 Queue::Submit 里、不随负载变化的阻塞。这是 JS 复刻件复刻不到的东西:
// 复刻件自己调 requestAdapter,拿到的未必是 wgpu 拿到的那块。
import { test } from '@playwright/test';
import { WEB_PORT } from '../config';

const BASE = `http://127.0.0.1:${WEB_PORT}`;

const CAPTURE = `
  window.__adapters = [];
  const orig = navigator.gpu.requestAdapter.bind(navigator.gpu);
  navigator.gpu.requestAdapter = async (opts) => {
    const a = await orig(opts);
    window.__adapters.push({
      opts: JSON.stringify(opts ?? {}),
      info: a ? { vendor: a.info?.vendor, architecture: a.info?.architecture,
                  device: a.info?.device, description: a.info?.description } : null,
    });
    return a;
  };
`;

test('两页拿到的 adapter', async ({ page }) => {
  await page.addInitScript(CAPTURE);
  await page.goto(`${BASE}/?tab=2&bevy=off`);
  await page.waitForTimeout(12_000);
  const app = await page.evaluate(
    () => (window as unknown as { __adapters: unknown[] }).__adapters,
  );
  console.log(`应用页 requestAdapter:\n${JSON.stringify(app, null, 2)}`);

  await page.goto(`${BASE}/rafprobe.html`);
  const probe = await page.evaluate(async () => {
    const out = [];
    for (const opts of [undefined, { powerPreference: 'high-performance' as const }, { powerPreference: 'low-power' as const }]) {
      const a = await navigator.gpu.requestAdapter(opts);
      out.push({
        opts: JSON.stringify(opts ?? {}),
        info: a ? { vendor: a.info?.vendor, architecture: a.info?.architecture,
                    device: a.info?.device, description: a.info?.description } : null,
      });
    }
    return out;
  });
  console.log(`对照页 requestAdapter:\n${JSON.stringify(probe, null, 2)}`);
});
