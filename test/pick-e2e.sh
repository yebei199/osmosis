#!/usr/bin/env bash
# 端到端:在界面上点一首歌,断言它真的起播了,而且只发布了一次队列(#113)。
#
#   test/pick-e2e.sh list    列表那条路:切到列表视图,点一行
#   test/pick-e2e.sh wall    卡墙那条路:切到卡墙,挪选中、确认
#   test/pick-e2e.sh search  搜索那条路:搜 KEYWORD,点一行结果
#   test/pick-e2e.sh queue   队列页那条路:展开播放页、打开队列,点一行(要已经在放)
#
# 两条都连点三下 —— 真机上第一下到出声要一两秒,用户本能地会再点(#125),
# 而遥控时连点曾经一下一次发布、两秒十发(#113)。
#
# 驱动走应用内嵌的 MCP,断言走数据库,不靠人看画面也不靠人去点:
#   - play_events 多了**恰好一行**:有一台设备真的起播了;
#   - 各设备队列的版本号之和最多涨 **1**:新的一批发布一次;这一批早已同步上去时
#     不再发布(#137 ③),那就必须看得见这一下记了检查点(play_queue_reports)。
#
# 组内(#142):设 OTHER_PORT 为组里另一台的 MCP 端口(真机经 adb forward 是 8090),
# 两台事先已在同一个组里(输出设备那一排按「+」)。组里点歌只改服务端的全局状态,
# 判据换成:play_events 恰好 +1(服务端记的)、play_groups.version 恰好 +1(连点只发
# 一次意图),并且 30 秒内**两台**控制条上的曲名都换成全局状态里那一条的曲名。
#
# 前提:应用起着(桌面 just desktop-dev,安卓 just mcp-android)、已登录
# (test/mcp-login.sh),just server-dev 与 osmosis-pg 在跑,每日推荐有歌。
set -euo pipefail

MODE="${1:?用法: $0 list|wall|search|queue}"
PORT="${PORT:-8091}"
OTHER_PORT="${OTHER_PORT:-}"
KEYWORD="${KEYWORD:-晴天}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"
TAPS=3

case "$MODE" in
  list) toggle="WallView::view-list-btn" ;;
  wall) toggle="WallView::view-wall-btn" ;;
  search|queue) toggle="WallView::view-list-btn" ;;
  *) echo "用法: $0 list|wall|search|queue" >&2; exit 2 ;;
esac

call() {
  curl -s -X POST "http://127.0.0.1:${CALL_PORT:-$PORT}/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

# 第 n 个匹配元素的句柄;一个都没有就返回空串。
#
# 从窗口根往下按 id 找,不用 find_elements_by_id:那个找不到 `if` / `for` 里长出来的元素
# (搜索框、分区条、播放页的队列入口都是),找不到就返回空,看着像页面不对(#142 F-7)。
handle() {
  call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}" \
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

# 某一台控制条上的曲名(PlayerBar::title 那个 Text 的无障碍标签)。
title_on() {
  local w r h
  w=$(CALL_PORT=$1 call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
  r=$(CALL_PORT=$1 call get_window_properties "{\"windowHandle\":$w}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')
  h=$(CALL_PORT=$1 call query_element_descendants "{\"elementHandle\":$r,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"PlayerBar::title\"}]}" \
    | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")')
  [ -n "$h" ] || return 0
  CALL_PORT=$1 call get_element_properties "{\"elementHandle\":$h}" \
    | python3 -c 'import json,sys; p=json.load(sys.stdin); print(p.get("accessibleLabel") or p.get("accessibleValue") or "")'
}

sql() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'
}
played() { sql "select count(*) from play_events;"; }
published() { sql "select coalesce(sum(revision), 0) from play_queues;"; }
# 最近一次检查点落库的时刻。同一批已经同步过时,点歌不再发布新版本,只记一个
# 检查点(#137 ③),账本上看得见的就是这一行。
checkpointed() { sql "select coalesce(max(reported_at)::text, '') from play_queue_reports;"; }
# 组的全局状态:版本号之和(一个账号一行),以及此刻那一条的曲名。
group_version() { sql "select coalesce(sum(version), 0) from play_groups;"; }
group_title() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "select e.title from play_groups g
    join play_queue_entries e on (e.queue_id, e.revision, e.entry_id) = (g.queue_id, g.revision, g.entry_id)
    order by g.version desc limit 1;" | sed 's/^ *//;s/ *$//'
}

# 组内:两台控制条上的曲名 30 秒内都换成全局状态里那一条。
both_follow() {
  local want; want=$(group_title)
  [ -n "$want" ] || { echo "$MODE: 失败 —— 库里的组没有在放的那一条" >&2; exit 1; }
  for _ in $(seq 1 30); do
    [ "$(title_on "$PORT")" = "$want" ] && [ "$(title_on "$OTHER_PORT")" = "$want" ] && {
      echo "  两台都换成了「$want」"; return 0; }
    sleep 1
  done
  echo "$MODE: 失败 —— 30 秒内两台没都换成「$want」(本机「$(title_on "$PORT")」,另一台「$(title_on "$OTHER_PORT")」)" >&2
  exit 1
}

must() {
  [ -n "$1" ] || { echo "找不到 $2 —— 页面不对,或者这个构建没有它" >&2; exit 1; }
}

# 切到要测的那个视图。没有卡墙的构建(非 GPU)只有列表,也就没有开关。
show_view() {
  # 卡墙起播后会开播放页,而播放页开着时卡墙不推相机(渲染循环只在墙露着
  # 时走它),点了也不会起播。用户是按返回/Esc 收起的,这里也一样。
  call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\u001b\"}" >/dev/null
  sleep 1
  local button
  button=$(handle "$toggle")
  if [ -n "$button" ]; then
    act "$button" Default_
  elif [ "$MODE" = wall ]; then
    must "" "$toggle"
  fi
  # 塌回 / 展开动画在渲染循环里走,落地才换视图。窗口不可见(滚出屏幕、
  # 最小化、熄屏)时合成器不派发重绘,动画停在半路,视图永远不换 —— 开关
  # 已经高亮,看着像「点了不切」。所以等目标视图真出现,等不到就说清楚。
  local want
  [ "$MODE" = wall ] && want="WallView::wall-area" || want="TrackList::touch"
  # 一秒一帧时(见下面起播那段)塌回要十几秒,等宽一点。
  for _ in $(seq 1 60); do
    [ -n "$(handle "$want")" ] && return
    sleep 1
  done
  echo "切到 $MODE 视图 60 秒没落地 —— 窗口可能不可见,动画不走" >&2
  exit 1
}

# 队列页里第 index 行。队列页常驻在播放页里,列表那几行与它同名,所以从 QueuePage 往下找。
queue_row() {
  local page
  page=$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":false,\"queryStack\":[{\"matchElementTypeName\":\"QueuePage\"}]}" \
    | python3 -c 'import json,sys; hs=json.load(sys.stdin).get("elementHandles") or []; print(json.dumps(hs[0]) if hs else "")')
  [ -n "$page" ] || return 0
  call query_element_descendants "{\"elementHandle\":$page,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"TrackList::touch\"}]}" \
    | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
print(json.dumps(hs[$1]) if len(hs) > $1 else '')
"
}

# 真机上填完搜索框,软键盘会盖住结果,第一下点击被它吃掉(#142 F-7)。键盘真的开着才按返回,
# 否则返回会把页面退掉。桌面(没有 adb 或不是真机那个端口)什么都不做。
hide_keyboard() {
  [ "${CALL_PORT:-$PORT}" = "${ANDROID_MCP_PORT:-8090}" ] && command -v adb >/dev/null || return 0
  sleep 1
  if adb shell dumpsys input_method | grep -q "mInputShown=true"; then
    adb shell input keyevent KEYCODE_BACK
    sleep 1
  fi
}

# 搜索分区搜 KEYWORD。结果落在列表里,之后与列表那条路一样点。
search() {
  local item box
  item=$(handle "MusicRail::item-touch" 2)
  [ -n "$item" ] || item=$(handle "MusicBar::item-touch" 2)
  must "$item" "搜索分区"
  call click_element "{\"elementHandle\":$item}" >/dev/null
  sleep 1
  box=$(handle "MusicPage::keyword")
  must "$box" "搜索框"
  call click_element "{\"elementHandle\":$box}" >/dev/null
  call set_element_value "{\"elementHandle\":$box,\"value\":\"$KEYWORD\"}" >/dev/null
  call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\n\"}" >/dev/null
  hide_keyboard
  for _ in $(seq 1 20); do
    [ -n "$(handle "TrackList::touch")" ] && return
    sleep 1
  done
  echo "搜「$KEYWORD」20 秒没有结果" >&2
  exit 1
}

# 展开播放页、打开队列页。
open_queue() {
  local cover entry
  cover=$(handle "PlayerBar::cover-touch")
  must "$cover" "控制条封面(队列页要已经在放)"
  call click_element "{\"elementHandle\":$cover}" >/dev/null
  sleep 2
  entry=$(handle "PlayPage::queue-entry-touch")
  must "$entry" "播放页的队列入口"
  call click_element "{\"elementHandle\":$entry}" >/dev/null
  sleep 2
}

# 连点第 index 首 TAPS 下。
tap() {
  local index=$1 target
  case "$MODE" in
    queue)
      target=$(queue_row "$index")
      must "$target" "队列页第 $((index + 1)) 行"
      for _ in $(seq "$TAPS"); do
        call click_element "{\"elementHandle\":$target}" >/dev/null
      done
      ;;
    list|search)
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
root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')

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
[ "$MODE" = search ] && search

for index in 0 1; do
  if [ "$MODE" = queue ]; then open_queue; elif [ "$MODE" != search ]; then show_view; fi
  played_before=$(played)
  version_before=$(group_version)
  published_before=$(published)
  checkpoint_before=$(checkpointed)
  tap "$index"
  echo "$MODE: 第 $((index + 1)) 首连点 $TAPS 下"

  # 卡墙要等相机推完才起播;取直链、开流、解码还要一两秒。等得宽:窗口
  # 不在前台(锁屏、熄屏)时合成器一秒只给一帧,相机要推二十多秒 —— 等短了,
  # 这一轮迟到的起播会漏进下一轮的账。
  for _ in $(seq 1 60); do
    sleep 1
    [ "$(played)" -gt "$played_before" ] && break
  done
  if [ "$(played)" -eq "$played_before" ]; then
    echo "  没起播,换下一首(点中的可能正是在放的那首)"
    continue
  fi
  if [ -n "$OTHER_PORT" ]; then
    # 组里:意图同步提交,起播记账时版本早已落库;多等一会儿,迟到的第二发才判得出来。
    sleep 3
    plays=$(( $(played) - played_before ))
    versions=$(( $(group_version) - version_before ))
    echo "  play_events +$plays,组版本 +$versions"
    if [ "$plays" -eq 1 ] && [ "$versions" -eq 1 ]; then
      both_follow
      echo "$MODE: 通过"
      exit 0
    fi
    echo "$MODE: 失败 —— 组里点一次该恰好起播一次、组版本恰好涨一次" >&2
    exit 1
  fi
  # 发布在起播之后异步做,先等它落地;再多等一会儿,迟到的第二发才判得出来。
  for _ in $(seq 1 10); do
    [ "$(published)" -gt "$published_before" ] && break
    sleep 1
  done
  sleep 3
  plays=$(( $(played) - played_before ))
  publishes=$(( $(published) - published_before ))
  [ "$(checkpointed)" != "$checkpoint_before" ] && checkpoint=有 || checkpoint=无
  echo "  play_events +$plays,队列发布 +$publishes,检查点$checkpoint"
  # 发布恰好一次:这一批是新的。发布零次:这一批早已同步上去,那就必须看得见
  # 这一下记了检查点 —— 否则就是该发布的没发布。两次以上是 #113/#125 回来了。
  if [ "$plays" -eq 1 ] && { [ "$publishes" -eq 1 ] || { [ "$publishes" -eq 0 ] && [ "$checkpoint" = 有 ]; }; }; then
    echo "$MODE: 通过"
    exit 0
  fi
  echo "$MODE: 失败 —— 点一次该恰好起播一次,并且发布一次(新批)或只记检查点(已同步的批)" >&2
  exit 1
done

echo "$MODE: 两首都没起播 —— 失败" >&2
exit 1
