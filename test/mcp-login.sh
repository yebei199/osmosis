#!/usr/bin/env bash
# 用应用内嵌的 MCP 把界面登进去。played-e2e.sh 假定「已登录」,这份补上那一步。
#
# 凭据从 .env 读,全程只经过 shell 变量,不落进命令行、不打印。
# 端口默认 8090(安卓;桌面传 PORT=8091)。已登录时直接返回,可重复跑。
set -euo pipefail

PORT="${PORT:-8090}"

set -a
# shellcheck disable=SC1091
source "$(dirname "$0")/../.env"
set +a
: "${TEST_USERNAME:?.env 里缺 TEST_USERNAME}"
: "${TEST_PASSWORD:?.env 里缺 TEST_PASSWORD}"

call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

# 第 n 个匹配元素的句柄;一个都没有就返回空串(调用方据此判断页面在不在)。
handle() {
  call find_elements_by_id "{\"windowHandle\":$win,\"elementsId\":\"$1\"}" \
  | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
print(json.dumps(hs[${2:-0}]) if len(hs) > ${2:-0} else '')
"
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')

user=$(handle "LoginPage::username")
if [ -z "$user" ]; then
  echo "登录页不在,当作已登录"
  exit 0
fi
pass=$(handle "LoginPage::password")

# 密码框的值经 set_element_value 进程序,不走命令行参数,也不回显。
call set_element_value "$(python3 -c '
import json, os, sys
print(json.dumps({"elementHandle": json.loads(sys.argv[1]), "value": os.environ["TEST_USERNAME"]}))
' "$user")" >/dev/null
call set_element_value "$(python3 -c '
import json, os, sys
print(json.dumps({"elementHandle": json.loads(sys.argv[1]), "value": os.environ["TEST_PASSWORD"]}))
' "$pass")" >/dev/null

btn=$(call query_element_descendants \
  "{\"elementHandle\":$win,\"findAll\":true,\"queryStack\":[{\"matchDescendants\":true},{\"matchElementId\":\"HoverButton::touch\"}]}" \
  | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")')
[ -n "$btn" ] || { echo "找不到登录按钮" >&2; exit 1; }
call click_element "{\"elementHandle\":$btn}" >/dev/null

# 登录走网络,给它几秒;判据是登录页消失,不是看画面。
for _ in $(seq 1 15); do
  sleep 1
  if [ -z "$(handle 'LoginPage::username')" ]; then
    echo "已登录"
    exit 0
  fi
done

echo "15 秒后登录页仍在 —— 失败" >&2
exit 1
