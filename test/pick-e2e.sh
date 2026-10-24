#!/usr/bin/env bash
# 兼容原入口，场景与公共 MCP 库由 uv 管理。
exec uv run --project "$(dirname "$0")/mcp-e2e" python "$(dirname "$0")/mcp-e2e/pick.py" "$@"
