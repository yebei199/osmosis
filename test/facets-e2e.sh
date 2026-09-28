#!/usr/bin/env bash
# 端到端:歌单视图的分组与筛选(#160)。
#
#   test/facets-e2e.sh liked   「我喜欢的」切「按歌手」:堆数 == 库里这个歌单的歌手去重数
#   test/facets-e2e.sh wall    日推卡墙:筛选时卡墙还在;一选分组就切回列表;
#                              再切回卡墙,分组清掉
#
# 驱动走应用内嵌的 MCP(桌面 8091 / 真机 8090),按元素 id 找、按无障碍动作按;
# 断言走数据库与元素树,不看截图。堆数读选中那颗分组药丸的无障碍描述
# (「12 堆」):列表是虚拟化的,滚不到的堆头不在元素树上,数行数不准。
#
# 前提:应用起着(桌面 just desktop-dev,安卓 just mcp-android)、已登录
# (test/mcp-login.sh)、just server-dev 与 osmosis-pg 在跑;wall 要 GPU 构建。
# 登录的账号默认取 .env 的 TEST_USERNAME,别的账号设 ACCOUNT。
set -euo pipefail

MODE="${1:?用法: $0 liked|wall}"
PORT="${PORT:-8091}"
PG_CONTAINER="${PG_CONTAINER:-osmosis-pg}"

if [ -z "${ACCOUNT:-}" ]; then
  set -a
  # shellcheck disable=SC1091
  source "$(dirname "$0")/../.env"
  set +a
  ACCOUNT="${TEST_USERNAME:?.env 里缺 TEST_USERNAME,或者设 ACCOUNT}"
fi

call() {
  curl -s -X POST "http://127.0.0.1:${PORT}/mcp" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}" \
  | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]["content"][0]; print(r.get("text",""))'
}

# 第 n 个匹配元素的句柄,从窗口根往下找(`if`/`for` 里长出来的也找得到)。
handle() {
  call query_element_descendants "{\"elementHandle\":$root,\"findAll\":true,\"queryStack\":[{\"matchElementId\":\"$1\"}]}" \
  | python3 -c "
import json, sys
hs = json.load(sys.stdin).get('elementHandles') or []
print(json.dumps(hs[${2:-0}]) if len(hs) > ${2:-0} else '')
"
}

must() {
  [ -n "$1" ] || { echo "找不到 $2 —— 页面不对,或者这个构建没有它" >&2; exit 1; }
}

act() {
  call invoke_accessibility_action "{\"elementHandle\":$1,\"action\":\"Default_\"}" >/dev/null
}

prop() {
  call get_element_properties "{\"elementHandle\":$1}" \
    | python3 -c "import json,sys; print(json.load(sys.stdin).get('$2') or '')"
}

sql() {
  docker exec "$PG_CONTAINER" psql -U slint -d osmosis -tAc "$1" | tr -d '[:space:]'
}

# 等某个 id 出现(want=yes)或消失(want=no),最多 30 秒。
wait_for() {
  local id=$1 want=$2
  for _ in $(seq 1 30); do
    if [ -n "$(handle "$id")" ]; then
      [ "$want" = yes ] && return 0
    else
      [ "$want" = no ] && return 0
    fi
    sleep 1
  done
  echo "$MODE: 失败 —— 30 秒内 $id 没有$([ "$want" = yes ] && echo 出现 || echo 消失)" >&2
  exit 1
}

# 按「歌手」分组(分组条第 2 颗:第 1 颗是「不分组」),读出分了几堆。
group_by_artist() {
  local pill
  pill=$(handle "FacetBar::grouping-pill" 1)
  must "$pill" "分组条的「歌手」"
  [ "$(prop "$pill" accessibleLabel)" = 歌手 ] \
    || { echo "分组条第 2 颗不是「歌手」,读到「$(prop "$pill" accessibleLabel)」" >&2; exit 1; }
  act "$pill"
  sleep 1
  prop "$pill" accessibleDescription | sed 's/ 堆$//'
}

win=$(call list_windows '{}' | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["windowHandles"][0]))')
root=$(call get_window_properties "{\"windowHandle\":$win}" | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["rootElementHandle"]))')

# 复位:收起播放页,进音乐页的第 $1 个分区(按摆放位置:0 每日推荐、2 我的歌单)。
reset() {
  local item music
  for _ in 1 2 3; do
    [ -n "$(call query_element_descendants "{\"elementHandle\":$root,\"findAll\":false,\"queryStack\":[{\"matchElementTypeName\":\"PlayPage\"}]}" \
      | python3 -c 'import json,sys; print("y" if json.load(sys.stdin).get("elementHandles") else "")')" ] || break
    call dispatch_key_event "{\"windowHandle\":$win,\"text\":\"\\u001b\"}" >/dev/null
    sleep 1
  done
  music=$(handle "NavItem::touch" 1)
  must "$music" "音乐入口"
  call click_element "{\"elementHandle\":$music}" >/dev/null
  item=$(handle "MusicRail::item-touch" "$1")
  [ -n "$item" ] || item=$(handle "MusicBar::item-touch" "$1")
  must "$item" "音乐页第 $(($1 + 1)) 个分区"
  call click_element "{\"elementHandle\":$item}" >/dev/null
  sleep 2
}

case "$MODE" in
  liked)
    reset 2
    liked=$(handle "PlaylistList::touch" 0)
    must "$liked" "歌单列表第一行(「我喜欢的」)"
    act "$liked"
    wait_for "TrackList::touch" yes
    wait_for "FacetBar::grouping-pill" yes
    # 列表先摆缓存那份,网络那份回来会再换一次;等它稳住
    sleep 5
    got=$(group_by_artist)
    # 界面上的规则:多歌手的歌进每一位的堆,没有歌手的归「未知歌手」一堆
    want=$(sql "select count(distinct a) from local_playlist_tracks t
      join local_playlists p on p.id = t.playlist_id
      join accounts ac on ac.id = p.account_id
      join platform_tracks d on d.platform = t.platform and d.track_id = t.track_id,
      unnest(coalesce(nullif(d.artists, '{}'), array['未知歌手'])) as a
      where lower(ac.username) = lower('$ACCOUNT') and p.system = 'liked';")
    [ -n "$got" ] && [ "$got" = "$want" ] \
      || { echo "liked: 失败 —— 界面分了「$got」堆,库里歌手去重是 $want" >&2; exit 1; }
    echo "  「我喜欢的」按歌手:$got 堆,库里歌手去重 $want"
    ;;
  wall)
    reset 0
    wall_btn=$(handle "WallView::view-wall-btn")
    must "$wall_btn" "卡墙开关(非 GPU 构建没有卡墙)"
    act "$wall_btn"
    wait_for "WallView::wall-area" yes
    wait_for "FacetBar::grouping-pill" yes

    # 卡墙能筛:打开筛选、选第一个 chip,卡墙还在
    act "$(handle "FacetBar::filter-toggle")"
    sleep 1
    # chip 的模型每按一次就重建,句柄随之失效:先读标签,按完重新取
    chip=$(handle "FacetBar::chip-pill" 0)
    must "$chip" "第一个筛选 chip"
    label=$(prop "$chip" accessibleLabel)
    act "$chip"
    sleep 2
    [ "$(prop "$(handle "FacetBar::filter-toggle")" accessibleLabel)" = "筛选 · 1" ] \
      || { echo "wall: 失败 —— 按了「$label」,筛选开关没报「筛选 · 1」" >&2; exit 1; }
    [ -n "$(handle "WallView::wall-area")" ] \
      || { echo "wall: 失败 —— 筛选之后卡墙没了" >&2; exit 1; }
    echo "  卡墙上筛「$label」:卡墙还在"
    act "$(handle "FacetBar::chip-pill" 0)"
    sleep 1
    [ "$(prop "$(handle "FacetBar::filter-toggle")" accessibleLabel)" = "筛选" ] \
      || { echo "wall: 失败 —— 再按一次「$label」没取消" >&2; exit 1; }

    # 一选分组就切回列表
    piles=$(group_by_artist)
    wait_for "WallView::wall-area" no
    wait_for "TrackList::touch" yes
    echo "  卡墙上选「歌手」:切回列表,分了 $piles 堆"

    # 再切回卡墙,分组清掉
    act "$(handle "WallView::view-wall-btn")"
    wait_for "WallView::wall-area" yes
    none=$(handle "FacetBar::grouping-pill" 0)
    [ "$(prop "$none" accessibleChecked)" = True ] \
      || { echo "wall: 失败 —— 切回卡墙后分组没清(「不分组」没选中)" >&2; exit 1; }
    echo "  切回卡墙:分组回到「不分组」"
    act "$(handle "FacetBar::filter-toggle")"
    ;;
  *) echo "用法: $0 liked|wall" >&2; exit 2 ;;
esac

echo "通过"
