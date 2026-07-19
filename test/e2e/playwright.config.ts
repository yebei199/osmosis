import { execSync } from 'node:child_process';
import { defineConfig } from '@playwright/test';

// Playwright 的 channel: 'chrome' 写死了 /opt/google/chrome/chrome,NixOS 上不存在。
// 从 PATH 里找,找不到再让 PLAYWRIGHT_CHROME 兜底。
const chromePath =
  process.env.PLAYWRIGHT_CHROME ??
  execSync(
    'command -v google-chrome-stable || command -v google-chrome',
    {
      shell: '/bin/sh',
    },
  )
    .toString()
    .trim();

// 端口与 justfile 的 web_port 一致。改一处就得改另一处 —— 两边都写了理由。
export const WEB_PORT = 8073;

export default defineConfig({
  testDir: '.',
  outputDir: 'test-results',
  // 帧率测量抢 GPU,并行跑等于互相污染。
  fullyParallel: false,
  workers: 1,
  reporter: 'list',
  // 一次运行要录两组 5s 的 trace,还要等 wasm 加载。
  timeout: 180_000,
  use: {
    browserName: 'chromium',
    // 必须有头:headless 的 Chrome 没有 WebGPU,requestAdapter 恒为 null。
    headless: false,
    // 用系统装的 Chrome,不用 Playwright 自带的 Chromium:NixOS 上那些预编译二进制
    // 跑不起来,而且 dist/ 里的历史 trace 都出自系统 Chrome,同源才好对齐。
    launchOptions: { executablePath: chromePath },
  },
  webServer: {
    command: `python3 apps/web/dev-server.py ${WEB_PORT} dist/web`,
    cwd: '../..',
    url: `http://127.0.0.1:${WEB_PORT}/rafprobe.html`,
    reuseExistingServer: true,
    stdout: 'pipe',
  },
});
