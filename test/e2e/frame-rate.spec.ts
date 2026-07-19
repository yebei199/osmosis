// web 端 3D 页的帧成本,对着同一台机器的天花板量。
//
// 绝对阈值(">= 100fps")在这里没有意义:数字取决于显示器刷新率和这块 GPU,换台机器就红。
// 所以每次运行先跑一个对照组 —— 一张什么都不干、只 clear 一次的 WebGPU canvas,形态复刻
// Slint 页(尺寸、CSS 缩放、rgba8unorm、opaque、每帧 2 次 submit)。它给出"这台机器上
// 一个 WebGPU 页面能跑多快、GPU 该花多少",应用再跟它比。
//
// 对照组同时是哨兵。三个条件缺一不可:有头(headless 的 Chrome 没有 WebGPU)、窗口真在
// 前台(后台标签页的 rAF 会被压到 1Hz —— 实测过,一开始就栽在这)、真 GPU。任何一条不满足,
// 对照组自己就跑不到刷新率,此时应当 skip 而不是报一个假的红。
import { expect, test } from '@playwright/test';
import { WEB_PORT } from './config';
import { format, recordFrames } from './trace';

const BASE = `http://127.0.0.1:${WEB_PORT}`;
const SAMPLE_SECONDS = 5;

// 对照组低于这个数就说明环境不成立(窗口没在前台 / 没有 WebGPU),测出来的数不能用。
const CONTROL_MIN_FPS = 100;
// 应用相对对照组的底线。当前实测约 0.39,离得很远 —— 见 docs/wasm/frame-rate.md。
const MIN_FPS_RATIO = 0.7;
// 每帧 GPU 时间相对对照组的上限。当前实测约 7 倍。
const MAX_GPU_RATIO = 3;

/** 在当前页面里起一个持续的 WebGPU 呈现循环,形态复刻 Slint 页。 */
async function startControlLoop(
  page: import('@playwright/test').Page,
) {
  await page.evaluate(async () => {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) throw new Error('没有 WebGPU adapter');
    const dev = await adapter.requestDevice();
    const canvas = document.createElement('canvas');
    canvas.width = 1605;
    canvas.height = 1984;
    canvas.style.width = '1284px';
    canvas.style.height = '1587px';
    document.body.appendChild(canvas);
    const ctx = canvas.getContext(
      'webgpu',
    ) as GPUCanvasContext | null;
    if (!ctx) throw new Error('拿不到 webgpu context');
    ctx.configure({
      device: dev,
      format: 'rgba8unorm',
      alphaMode: 'opaque',
    });
    (function loop() {
      // 每帧 2 次 submit:Slint 页实测就是 2 次,第二次是空命令缓冲。
      for (let i = 0; i < 2; i++) {
        const enc = dev.createCommandEncoder();
        if (i === 0) {
          const pass = enc.beginRenderPass({
            colorAttachments: [
              {
                view: ctx.getCurrentTexture().createView(),
                clearValue: {
                  r: 0.1,
                  g: 0.15,
                  b: 0.3,
                  a: 1,
                },
                loadOp: 'clear',
                storeOp: 'store',
              },
            ],
          });
          pass.end();
        }
        dev.queue.submit([enc.finish()]);
      }
      requestAnimationFrame(loop);
    })();
  });
}

test('3D 页的帧成本不该远超同机的 WebGPU 天花板', async ({
  page,
}, testInfo) => {
  // ── 对照组 ──
  await page.goto(`${BASE}/rafprobe.html`);
  await startControlLoop(page);
  await page.waitForTimeout(2000); // 进稳态再采
  const control = await recordFrames(page, SAMPLE_SECONDS);
  testInfo.annotations.push({
    type: '对照组',
    description: format('rafprobe', control),
  });

  test.skip(
    control.fps < CONTROL_MIN_FPS,
    `对照组只有 ${control.fps.toFixed(1)}fps —— 窗口没在前台,或这台机器没有 WebGPU。` +
      '这个测试需要有头浏览器 + 窗口可见 + 真 GPU。',
  );

  // ── 被测组:应用的 3D 页 ──
  await page.goto(`${BASE}/?tab=2`);
  // wasm 有 34MB,加载 + 建 device 要一会儿。等到画面真的在动为止。
  await page.waitForFunction(
    () =>
      new Promise<boolean>((r) =>
        requestAnimationFrame(() =>
          requestAnimationFrame(() => r(true)),
        ),
      ),
    undefined,
    { timeout: 60_000 },
  );
  await page.waitForTimeout(5000);
  const app = await recordFrames(page, SAMPLE_SECONDS);
  testInfo.annotations.push({
    type: '被测组',
    description: format('应用 3D 页', app),
  });

  const fpsRatio = app.fps / control.fps;
  const gpuRatio =
    app.gpuMsPerFrame / control.gpuMsPerFrame;
  console.log(
    `${format('对照组 rafprobe', control)}\n${format('被测组 应用 3D 页', app)}`,
  );
  console.log(
    `帧率比 ${(fpsRatio * 100).toFixed(0)}%(底线 ${MIN_FPS_RATIO * 100}%),` +
      `GPU 每帧 ${gpuRatio.toFixed(1)} 倍(上限 ${MAX_GPU_RATIO} 倍)`,
  );

  expect(
    fpsRatio,
    '应用帧率相对同机 WebGPU 天花板',
  ).toBeGreaterThanOrEqual(MIN_FPS_RATIO);
  expect(
    gpuRatio,
    '应用每帧 GPU 时间相对同机 WebGPU 天花板',
  ).toBeLessThanOrEqual(MAX_GPU_RATIO);
});
