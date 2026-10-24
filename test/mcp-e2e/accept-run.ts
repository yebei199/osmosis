// 为累计验收增加 MCP 库的集合；旧遥控集合仍交原入口执行。

import {
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// 固定以本脚本所在仓库解析路径，兼容调用者工作目录。
const root = resolve(
  fileURLToPath(new URL('../..', import.meta.url)),
);
// 只允许已登记的本轮测试模块，避免误把其他套件送进此目录。
const modules = new Set([
  'test_client',
  'test_visual',
  'test_scenarios',
  'test_acceptance',
  'test_local_app',
]);

// 子命令失败立即传播，不能产出部分通过的验收结果。
function run(command: string[]): void {
  const result = Bun.spawnSync(command, {
    cwd: root,
    env: process.env,
    stdout: 'inherit',
    stderr: 'inherit',
  });
  if (result.exitCode !== 0) process.exit(result.exitCode);
}

// 将 JUnit classname::name 转成当前 pytest 的文件节点。
function mcpNode(test: string): string | undefined {
  const [module, ...rest] = test.split('::');
  const name = module.replace(/\.py$/, '');
  if (!modules.has(name)) return undefined;
  return `test/mcp-e2e/${name}.py${rest.length ? `::${rest.join('::')}` : ''}`;
}

// 未选中套件以零用例表示，本轮未执行的旧结果不能混进判定。
function emptyResult(path: string): void {
  const file = resolve(root, path);
  if (!file.startsWith(`${root}/`))
    throw new Error(
      `JUnit path outside repository: ${path}`,
    );
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(
    file,
    '<?xml version="1.0" encoding="UTF-8"?><testsuites tests="0"/>',
  );
}

// 当前集合的真实 JUnit，不能被未选中套件的占位覆盖。
const localJunit = 'test/mcp-e2e/results/junit.xml';

// day gate 只执行点名集合；nightly 的 all 才调用旧累计入口。
const [mode, ...tests] = process.argv.slice(2);
if (
  mode !== 'all' &&
  (mode !== 'subset' || tests.length === 0)
) {
  console.error(
    'usage: accept-run.ts all | subset <tests...>; subset requires tests',
  );
  process.exit(2);
}
const local = tests
  .map(mcpNode)
  .filter((node): node is string => node !== undefined);
const remote = tests.filter(
  (test) => mcpNode(test) === undefined,
);
// 桌面使用与 desktop-dev 相同的指纹，防止验到旧二进制或缺元素调试信息。
const needsApp =
  mode === 'all' ||
  local.some(
    (node) =>
      node === 'test/mcp-e2e/test_scenarios.py' ||
      node.includes('::test_pick[') ||
      node.endsWith('::test_views') ||
      node.endsWith(
        '::test_database_and_state_belong_to_this_run',
      ),
  );
if (needsApp) {
  run([
    'nix-shell',
    'slint.nix',
    '--run',
    'env -u OSMOSIS_API_BASE SLINT_EMIT_DEBUG_INFO=1 SLINT_LIVE_PREVIEW=1 cargo build --locked -p app-desktop --features mcp,slint/live-preview',
  ]);
  run(['cargo', 'build', '--locked', '-p', 'server']);
}
if (mode === 'all' || local.length > 0) {
  run([
    'uv',
    'run',
    '--project',
    'test/mcp-e2e',
    'pytest',
    ...(mode === 'all' ? ['test/mcp-e2e'] : local),
    `--junitxml=${localJunit}`,
    '--mcp-artifacts',
    process.env.MCP_ARTIFACTS ??
      'test/mcp-e2e/results/artifacts',
  ]);
}
if (mode === 'all' || remote.length > 0) {
  run(['bash', 'acceptance/run.sh', mode, ...remote]);
}

if (mode !== 'all' && remote.length === 0) {
  const outputs = Bun.TOML.parse(
    readFileSync(
      resolve(root, 'acceptance/run.toml'),
      'utf8',
    ),
  ).junit;
  if (!Array.isArray(outputs))
    throw new Error(
      'acceptance/run.toml: junit must be an array',
    );
  for (const output of outputs) {
    if (typeof output !== 'string')
      throw new Error('JUnit path must be a string');
    if (output !== localJunit) emptyResult(output);
  }
}
if (mode !== 'all' && local.length === 0)
  emptyResult(localJunit);
