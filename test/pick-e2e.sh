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
#   - 各设备队列的版本号之和最多涨 **1**:新的一批发布一次;这一批早已同步上去时
#     不再发布(#137 ③),那就必须看得见这一下记了检查点(play_queue_reports)。
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
# 最近一次检查点落库的时刻。同一批已经同步过时,点歌不再发布新版本,只记一个
# 检查点(#137 ③),账本上看得见的就是这一行。
checkpointed() { sql "select coalesce(max(reported_at)::text, '') from play_queue_reports;"; }

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
  [ "$MODE" = list ] && want="TrackList::touch" || want="WallView::wall-area"
  # 一秒一帧时(见下面起播那段)塌回要十几秒,等宽一点。
  for _ in $(seq 1 60); do
    [ -n "$(handle "$want")" ] && return
    sleep 1
  done
  echo "切到 $MODE 视图 60 秒没落地 —— 窗口可能不可见,动画不走" >&2
  exit 1
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
