#!/usr/bin/env bash
# 端到端:屏蔽规则(#161)。四条,对着 issue 的验收步骤:
#
#   test/blocks-e2e.sh radio     红心一首、踩一首,心动以它们旁边那首为种子再拉一批,两首都不在
#   test/blocks-e2e.sh outlets   屏蔽一位歌手 → 本地歌单、日推、电台的返回里都没有他;删规则 → 回来
#   test/blocks-e2e.sh queue     界面上放日推,长按下一首「屏蔽此曲」,按「下一首」:跳过它
#   test/blocks-e2e.sh settings  设置页「已屏蔽」点「恢复」:规则少一条
#
# radio 与 outlets 走 HTTP:用 .env 的 TEST_USERNAME / TEST_PASSWORD 登本机后端拿 token,
# 断言看响应与数据库。queue 与 settings 走应用内嵌的 MCP(桌面 8091 / 真机 8090),
# 断言看数据库:block_rules 的行数、play_events 里有没有被屏蔽的那首。
#
# 前提:just server-dev、bang-dream、osmosis-pg 在跑,测试账号已绑网易云、日推有歌;
# queue / settings 另要应用起着且已登录(test/mcp-login.sh)。服务端连的不是 osmosis
# 库时用 PG_DB 指过去。
set -euo pipefail

MODE="${1:?用法: $0 radio|outlets|queue|settings}"
API="${API:-http://127.0.0.1:3000}"
PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
PG_DB="${PG_DB:-osmosis}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"

sql() {
  docker exec "$PG_CONTAINER" psql -U slint -d "$PG_DB" -tAc "$1" | tr -d '[:space:]'
}

fail() {
  echo "失败 —— $*" >&2
  exit 1
}

# ── HTTP ──

login() {
  set -a
  # shellcheck disable=SC1091
  . "$REPO/.env"
  set +a
  TOKEN=$(curl -s -X POST "$API/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$TEST_USERNAME\",\"password\":\"$TEST_PASSWORD\"}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("token",""))')
  [ -n "$TOKEN" ] || fail "登不上本机后端 $API"
  ACCOUNT=$(sql "select id from accounts where username = '$TEST_USERNAME';")
}

http() {
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -sf -X "$method" "$API$path" -H "Authorization: Bearer $TOKEN" \
      -H 'Content-Type: application/json' -d "$body"
  else
    curl -sf -X "$method" "$API$path" -H "Authorization: Bearer $TOKEN"
  fi
}

# 一批曲目的 id,一行一个。
ids() {
  python3 -c 'import json,sys; [print(t["id"]) for t in json.load(sys.stdin)["tracks"]]'
}

radio() {
  login
  local batch seed a b neighbour again
  seed=$(http GET /daily | ids | head -1)
  [ -n "$seed" ] || fail "日推是空的,没有种子"
  batch=$(http GET "/radio?mode=heart&seed=$seed" | ids)
  a=$(echo "$batch" | sed -n 1p)
  b=$(echo "$batch" | sed -n 2p)
  neighbour=$(echo "$batch" | sed -n 3p)
  [ -n "$a" ] && [ -n "$b" ] || fail "心动第一批不到两首,挑不出红心与踩的对象"

  http PUT "/liked/$a" >/dev/null
  http PUT "/feedback/$b" '{"verdict":-1}' >/dev/null
  # 收尾撤掉这两下。trap 在函数返回后才跑,局部变量那时已经没了,所以现在就展开
  trap "http DELETE /liked/$a >/dev/null || true; http DELETE /feedback/$b >/dev/null || true" EXIT

  [ "$(sql "select count(*) from local_playlist_tracks t join local_playlists p on p.id = t.playlist_id
        where p.account_id = $ACCOUNT and p.system = 'liked' and t.track_id = '$a';")" = 1 ] \
    || fail "红心没落库($a)"
  [ "$(sql "select verdict from track_feedback where account_id = $ACCOUNT and track_id = '$b';")" = -1 ] \
    || fail "踩没落库($b)"
  echo "  红心 $a、踩 $b:库里都有"

  # 以挨着它们的那首为种子再拉一批
  again=$(http GET "/radio?mode=heart&seed=${neighbour:-$seed}" | ids)
  if echo "$again" | grep -qx "$a"; then fail "红心过的 $a 又出现在心动里"; fi
  if echo "$again" | grep -qx "$b"; then fail "踩过的 $b 又出现在心动里"; fi
  echo "  心动再拉 $(echo "$again" | grep -c .) 首:红心与踩的都不在"
  echo "通过"
}

outlets() {
  login
  local daily artist first list playlist rule path hidden
  daily=$(http GET /daily)
  first=$(echo "$daily" | ids | head -1)
  [ -n "$first" ] || fail "日推是空的"
  artist=$(echo "$daily" | python3 -c 'import json,sys; print(json.load(sys.stdin)["tracks"][0]["artists"][0])')

  playlist=$(http POST /playlists "{\"name\":\"e2e-blocks-$$\"}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  trap "http DELETE /playlists/local/$playlist >/dev/null || true" EXIT
  http POST "/playlists/local/$playlist/tracks" \
    "{\"tracks\":[{\"platform\":\"netease\",\"id\":\"$first\"}]}" >/dev/null

  rule=$(http POST /blocks "{\"kind\":\"artist\",\"value\":\"$artist\"}" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
  [ "$(sql "select count(*) from block_rules where id = $rule and account_id = $ACCOUNT;")" = 1 ] \
    || fail "规则没落库"
  echo "  屏蔽歌手「$artist」:block_rules 多一行"

  # 每个出口:没有这位歌手的歌,并报出藏了几首
  for path in /daily "/playlists/local/$playlist/tracks" "/radio?mode=fm"; do
    list=$(http GET "$path")
    echo "$list" | ARTIST="$artist" python3 -c '
import json, os, sys
d = json.load(sys.stdin)
sys.exit(1 if any(os.environ["ARTIST"] in t["artists"] for t in d["tracks"]) else 0)' \
      || fail "$path 的返回里还有「$artist」的歌"
    hidden=$(echo "$list" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("hidden",0))')
    echo "  $path:没有他的歌(hidden=$hidden)"
  done
  [ "$(http GET /daily | python3 -c 'import json,sys; print(json.load(sys.stdin)["hidden"])')" -ge 1 ] \
    || fail "日推该报出至少藏了一首"

  http DELETE "/blocks/$rule" >/dev/null
  http GET /daily | ids | grep -qx "$first" || fail "删了规则,$first 没回到日推"
  http GET "/playlists/local/$playlist/tracks" | ids | grep -qx "$first" \
    || fail "删了规则,$first 没回到歌单"
  echo "  删掉规则:日推与歌单里 $first 回来了"
  echo "通过"
}

# ── MCP ──

call() {
  curl -s -X POST "http://127.0.0.1:${PORT}/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

handle() {
  call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}" \
  | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
print(json.dumps(hs[${2:-0}]) if len(hs) > ${2:-0} else '')
"
}

must() {
  [ -n "$1" ] || fail "找不到 $2 —— 页面不对,或者这个构建没有它"
}

act() {
  call invoke_accessibility_action "{\"elementHandle\":$1,\"action\":\"Default_\"}" >/dev/null
}

# 无障碍标签以 $1 开头的第一个按钮。
labelled() {
  local h label
  for h in $(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementAccessibleRole\":\"Button\"}]}" \
    | python3 -c 'import json,sys; [print(json.dumps(h, separators=(",",":"))) for h in json.load(sys.stdin).get("elementHandles") or []]'); do
    label=$(call get_element_properties "{\"elementHandle\":$h}" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("accessibleLabel") or "")')
    case "$label" in "$1"*) echo "$h"; return ;; esac
  done
}

attach() {
  win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
  root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')
}

# 收起播放页、回到音乐页的每日推荐。
to_music() {
  local present music
  for _ in 1 2 3; do
    present=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":false,\"queryStack\":[{\"matchElementTypeName\":\"PlayPage\"}]}" \
      | python3 -c 'import json,sys; print("yes" if json.load(sys.stdin).get("elementHandles") else "")')
    [ -n "$present" ] || break
    call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\u001b\"}" >/dev/null
    sleep 1
  done
  music=$(handle "NavItem::touch" 1)
  must "$music" "音乐入口"
  call click_element "{\"elementHandle\":$music}" >/dev/null
  sleep 2
}

# 长按一行:拖到老远,插值步数撑够 450ms(见 test/tags-e2e.sh 文件头)。
long_press() {
  local props x y
  props=$(call get_element_properties "{\"elementHandle\":$1}")
  x=$(echo "$props" | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p["absolutePosition"]["x"] + p["size"]["width"]/2)')
  y=$(echo "$props" | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p["absolutePosition"]["y"] + p["size"]["height"]/2 + 5000)')
  call drag_element "{\"elementHandle\":$1,\"target\":{\"x\":$x,\"y\":$y},\"button\":\"Left\"}" >/dev/null
  sleep 1
}

queue() {
  attach
  to_music
  local first second rules_before blocker blocked since next
  first=$(handle "TrackList::touch" 0)
  second=$(handle "TrackList::touch" 1)
  must "$second" "列表第二行 —— 日推不到两首"
  call click_element "{\"elementHandle\":$first}" >/dev/null
  sleep 3

  rules_before=$(sql "select count(*) from block_rules;")
  long_press "$second"
  blocker=$(labelled "屏蔽此曲")
  must "$blocker" "长按菜单里的「屏蔽此曲」"
  act "$blocker"
  sleep 2
  [ "$(sql "select count(*) from block_rules;")" -eq "$((rules_before + 1))" ] \
    || fail "点了「屏蔽此曲」,block_rules 没多一行"
  blocked=$(sql "select value from block_rules where kind = 'track' order by id desc limit 1;")
  echo "  屏蔽第二首 $blocked:block_rules 多一行"

  since=$(docker exec "$PG_CONTAINER" psql -U slint -d "$PG_DB" -tAc "select now()" | xargs)
  next=$(labelled "下一首")
  must "$next" "控制条的「下一首」"
  act "$next"
  sleep 4

  [ "$(sql "select count(*) from play_events where track_id = '$blocked' and played_at >= '$since';")" = 0 ] \
    || fail "被屏蔽的 $blocked 还是起播了"
  [ "$(sql "select count(*) from play_events where played_at >= '$since';")" -ge 1 ] \
    || fail "按了「下一首」,一首都没起播"
  echo "  按「下一首」:跳过了 $blocked,起播的是它后面那首"
  echo "通过"
}

settings() {
  attach
  local nav before restore
  # 底栏(窄版式)四格都是 NavItem;宽版式的设置沉在侧栏底,是一颗标签为「设置」的圆钮
  nav=$(handle "NavItem::touch" 3)
  if [ -n "$nav" ]; then
    call click_element "{\"elementHandle\":$nav}" >/dev/null
  else
    nav=$(labelled "设置")
    must "$nav" "设置入口"
    act "$nav"
  fi
  sleep 2
  before=$(sql "select count(*) from block_rules;")
  [ "$before" -ge 1 ] || fail "库里没有规则可恢复,先跑一次 queue"
  restore=$(labelled "恢复 ")
  must "$restore" "设置页「已屏蔽」里的「恢复」"
  act "$restore"
  sleep 2
  [ "$(sql "select count(*) from block_rules;")" -eq "$((before - 1))" ] \
    || fail "点了「恢复」,block_rules 没少一行"
  echo "  设置页点「恢复」:block_rules 少一行"
  echo "通过"
}

case "$MODE" in
  radio) radio ;;
  outlets) outlets ;;
  queue) queue ;;
  settings) settings ;;
  *) echo "用法: $0 radio|outlets|queue|settings" >&2; exit 2 ;;
esac
