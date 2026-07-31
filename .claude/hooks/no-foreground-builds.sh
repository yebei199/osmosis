#!/usr/bin/env bash
# PreToolUse(Bash)钩子:拒绝在前台执行重构建命令。
#
# 背景:Bash 工具在命令返回前不吐任何输出,而本仓库的 nix-shell / 跨 target /
# bevy 构建都是分钟级起步。前台跑 = 用户面前一片静止的黑屏,看起来像死锁。
# 光靠 agent 自觉记得加 run_in_background 靠不住,故在这里硬拦。
#
# 退出码 2 = 阻止本次工具调用,stderr 回灌给模型让它改用后台重提交。
set -uo pipefail

input=$(cat)
cmd=$(jq -r '.tool_input.command // ""' <<<"$input")
bg=$(jq -r '.tool_input.run_in_background // false' <<<"$input")

# 已经是后台任务,放行。
[[ $bg == true ]] && exit 0

# 重构建特征:nix-shell 环境、跨 target 编译、bevy feature、整套 CI/打包 recipe。
# host target 的普通 cargo check / clippy(增量、秒级)不在此列,照常前台跑。
heavy='nix-shell|--target[ =](aarch64-linux-android|wasm32-unknown-unknown|aarch64-apple-ios)|cargo xtask android|bevy-3d|just +(ci|ci-test|ci-cross|ci-boundaries|shot|android-|mcp-android)'

if grep -Eq "$heavy" <<<"$cmd"; then
    echo "拒绝:这是重构建命令(nix-shell / 跨 target / bevy / just ci*),前台跑会让会话假死几分钟。请带 run_in_background: true 重新提交,再用 TaskOutput 或读 output 文件取结果。" >&2
    exit 2
fi

exit 0
