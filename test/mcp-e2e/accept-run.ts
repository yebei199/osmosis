// 为累计验收增加 MCP 库的集合；旧遥控集合仍交原入口执行。

import { resolve } from 'node:path';
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
if (mode === 'all' || local.length > 0) {
  run([
    'uv',
    'run',
    '--project',
    'test/mcp-e2e',
    'pytest',
    ...(mode === 'all' ? ['test/mcp-e2e'] : local),
    '--junitxml=test/mcp-e2e/results/junit.xml',
  ]);
}
if (mode === 'all' || remote.length > 0) {
  run(['bash', 'acceptance/run.sh', mode, ...remote]);
}
