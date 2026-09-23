#!/usr/bin/env bash
# 端到端:在界面上点一首歌,断言它真的起播了,而且只发布了一次队列(#113)。
#
#   test/pick-e2e.sh list    列表那条路:切到列表视图,点一行
#   test/pick-e2e.sh wall    卡墙那条路:切到卡墙,挪选中、确认
#
# 两条都连点三下 —— 真机上第一下到出声要一两秒,用户本能地会再点(#125),
# 而遥控时连点曾经一下一次发布、两秒十发(#113)。
#
# 驱动走应用内嵌的 MCP,断言走数据库,不靠人看画面也不靠人去点:
#   - play_events 多了**恰好一行**:有一台设备真的起播了;
#   - 各设备队列的版本号之和只涨了 **1**:这几下只发布了一次队列。
# 输出在本机还是遥控别的设备都适用 —— 起播记账的是真在放的那一台,队列归它。
#
# 前提:应用起着(桌面 just desktop-dev,安卓 just mcp-android)、已登录
# (test/mcp-login.sh),just server-dev 与 osmosis-pg 在跑,每日推荐有歌。
set -euo pipefail

MODE="${1:?用法: $0 list|wall}"
PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
TAPS=3

case "$MODE" in
  list) toggle="WallView::view-list-btn" ;;
  wall) toggle="WallView::view-wall-btn" ;;
  *) echo "用法: $0 list|wall" >&2; exit 2 ;;
esac

call() {
  curl -s -X POST "http://127.0.0.1:$PORT/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

# 第 n 个匹配元素的句柄;一个都没有就返回空串。
handle() {
  call find_elements_by_id "{\"windowHandle\":$win,\"elementsId\":\"$1\"}" \
  | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
print(json.dumps(hs[${2:-0}]) if len(hs) > ${2:-0} else '')
"
}

# 无障碍动作不过命中测试:卡墙的卡画在 3D 纹理里,按坐标点不稳(#113)。
act() {
  call invoke_accessibility_action "{\"elementHandle\":$1,\"action\":\"$2\"}" >/dev/null
}

sql() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'
}
played() { sql "select count(*) from play_events;"; }
published() { sql "select coalesce(sum(revision), 0) from play_queues;"; }

must() {
  [ -n "$1" ] || { echo "找不到 $2 —— 页面不对,或者这个构建没有它" >&2; exit 1; }
}

# 切到要测的那个视图。没有卡墙的构建(非 GPU)只有列表,也就没有开关。
show_view() {
  local button
  button=$(handle "$toggle")
  if [ -n "$button" ]; then
    act "$button" Default_
    sleep 2   # 塌回 / 展开动画落地才换视图
  elif [ "$MODE" = wall ]; then
    must "" "$toggle"
  fi
}

# 连点第 index 首 TAPS 下。
tap() {
  local index=$1 target
  case "$MODE" in
    list)
      target=$(handle "TrackList::touch" "$index")
      must "$target" "列表第 $((index + 1)) 行"
      for _ in $(seq "$TAPS"); do
        call click_element "{\"elementHandle\":$target}" >/dev/null
      done
      ;;
    wall)
      target=$(handle "WallView::wall-area")
      must "$target" "卡墙场区"
      # 往后挪 index+1 下。选中从哪起不要紧:点中哪首都行,两轮点的不是同一首就够。
      for _ in $(seq $((index + 1))); do act "$target" Increment; done
      for _ in $(seq "$TAPS"); do act "$target" Default_; done
      ;;
  esac
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')

# 音乐页 → 每日推荐。两种版式的分区条 id 不同,哪个在用哪个。
music=$(handle "NavItem::touch" 1)
must "$music" "音乐入口"
call click_element "{\"elementHandle\":$music}" >/dev/null
daily=$(handle "MusicRail::item-touch" 0)
[ -n "$daily" ] || daily=$(handle "MusicBar::item-touch" 0)
must "$daily" "每日推荐分区"
call click_element "{\"elementHandle\":$daily}" >/dev/null
sleep 2

# 点中的若正是在放的那首,界面按多余的点击丢掉它,账本自然不动 ——
# 所以两个候选轮着试,只固定点一首的话第二次跑必然失败。
for index in 0 1; do
  show_view
  played_before=$(played)
  published_before=$(published)
  tap "$index"
  echo "$MODE: 第 $((index + 1)) 首连点 $TAPS 下"

  # 卡墙要等相机推完才起播;取直链、开流、解码还要一两秒。
  for _ in $(seq 1 20); do
    sleep 1
    [ "$(played)" -gt "$played_before" ] && break
  done
  if [ "$(played)" -eq "$played_before" ]; then
    echo "  没起播,换下一首(点中的可能正是在放的那首)"
    continue
  fi
  # 迟到的第二发要有时间落地,才判得出它有没有。
  sleep 3
  plays=$(( $(played) - played_before ))
  publishes=$(( $(published) - published_before ))
  echo "  play_events +$plays,队列发布 +$publishes"
  if [ "$plays" -eq 1 ] && [ "$publishes" -eq 1 ]; then
    echo "$MODE: 通过"
    exit 0
  fi
  echo "$MODE: 失败 —— 点一次该恰好起播一次、发布一次" >&2
  exit 1
done

echo "$MODE: 两首都没起播 —— 失败" >&2
exit 1
