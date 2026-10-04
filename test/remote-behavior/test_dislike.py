"""#181：真实不喜欢入口 → 规则/点踩落库 → 列表、队列与实际音频。"""

import json
import re
import time
import urllib.request
from datetime import datetime
from enum import Enum

import pytest

import resources
from media import Media
from resources import wait_until
from test_remote import world as base_world  # noqa: F401

REASONS = {
    "artist": "不想再听这个歌手",
    "versions": "不想再听这首歌（含其他版本）",
    "exact": "太难听了",
}
# 重复 id 和其他专辑夹在当前曲目与下一首之间，不能只跳过旧 id。
CATALOG = {
    "175001": ("Song", (("7", "Artist A"), ("8", "Artist B"))),
    "175002": ("Song", (("7", "Artist A"),)),
    "175003": ("Song (Live)", (("7", "Artist A"),)),
    "175004": ("Other", (("7", "Artist A"),)),
    "175005": ("Song", (("9", "Artist C"),)),
    "175006": ("Safe", (("9", "Artist C"),)),
}
BLOCKED = {
    "artist": {"175001", "175002", "175003", "175004"},
    "versions": {"175001", "175002", "175003"},
    "exact": {"175001", "175002"},
}
NEXT = {"artist": "175005", "versions": "175004", "exact": "175003"}


# 固定为已发布旧客户端认识的三个值，不从新生产枚举动态生成。
class LegacyBlockKind(str, Enum):
    ARTIST = "artist"
    TAG = "tag"
    TRACK = "track"


def read_blocks(world, path):
    session = world.request("/login", {"username": world.username, "password": world.password})
    request = urllib.request.Request(
        f"http://127.0.0.1:{world.server_port}{path}",
        headers={"Authorization": "Bearer " + session["token"]},
    )
    with urllib.request.urlopen(request, timeout=8) as response:
        return json.load(response)["rules"]


# 只替换外部目录，声音仍由原有 WAV 字节独立编码曲目 id。
class DislikeMedia(Media):
    def track(self, track_id):
        track = super().track(track_id)
        if track_id in CATALOG:
            title, artists = CATALOG[track_id]
            track.title = title
            del track.artists[:]
            track.artists.extend(self.pb.Artist(id=key, name=name) for key, name in artists)
            track.album.id = "album-" + track_id
            track.album.name = "Album " + track_id
        return track

    def dispatch(self, method, request, context):
        if method in ("GetDailyRecommendations", "SearchTracks", "GetPersonalFm"):
            with self.lock:
                self.calls.append({"method": method, "request": str(request)})
            fields = {"tracks": [self.track(key) for key in CATALOG]}
            if method == "SearchTracks":
                fields["total"] = len(CATALOG)
            return getattr(self.pb, method + "Response")(**fields)
        return super().dispatch(method, request, context)


# 每个 World 仍持有并回收独有数据库、账户、namespace、音频和显示服务。
@pytest.fixture
def dislike_world(monkeypatch, request):
    monkeypatch.setattr(resources, "Media", DislikeMedia)
    return request.getfixturevalue("base_world")


def tree(ui):
    result = ui.call("get_element_tree", elementHandle=ui.root, maxElements=10000)
    assert not result["truncated"], "cannot assert absence using a truncated element tree"
    with (ui.client.directory / "dislike-trees.jsonl").open("a") as log:
        log.write(json.dumps(result) + "\n")
    return result["elements"]


# 标签是产品行为的断言；入口缺失明确失败，不伪装成环境 RED。
def button(ui, label):
    matches = [
        element["handle"]
        for element in tree(ui)
        if element.get("accessibleLabel") == label
        and element.get("accessibleRole") in ("Button", "Checkbox")
        and ui.visible(element["handle"])
    ]
    assert len(matches) == 1, f"expected one visible button {label!r}, got {len(matches)}"
    return matches[0]


def activate(ui, label):
    ui.activate(button(ui, label))


def rows(ui, scope=None):
    handles = (
        ui.elements("TrackList::touch")
        if scope is None
        else ui.call(
            "query_element_descendants",
            elementHandle=scope,
            findAll=True,
            queryStack=[{"matchElementId": "TrackList::touch"}],
        ).get("elementHandles", [])
    )
    return [
        ui.call("get_element_properties", elementHandle=handle)["accessibleLabel"]
        for handle in handles
        if ui.visible(handle)
    ]


def daily(ui):
    ui.music()
    ui.activate(ui.must("WallView::view-list-btn"))
    wait_until(lambda: rows(ui), "daily list ready")


# Expand 是列表上下文菜单的读屏入口，与真实长按共用生产回调。
def long_press(client):
    handle = client.ui.must("TrackList::touch", "Song")
    client.ui.activate(handle, action="Expand")


def cancel(client):
    # 在遮罩下面的导航位置发送真实指针点击，命中的是菜单外遮罩。
    handles = client.ui.elements("NavItem::touch")
    assert handles, "existing navigation is required for outside-click coordinates"
    client.ui.activate(handles[0], pointer=True)
    assert all(
        not any(item.get("accessibleLabel") == label for item in tree(client.ui))
        for label in REASONS.values()
    ), "outside click must close the reason menu"


def playing_title(ui, title):
    bars = ui.call(
        "query_element_descendants",
        elementHandle=ui.root,
        findAll=True,
        queryStack=[{"matchElementTypeName": "PlayerBar"}],
    ).get("elementHandles", [])
    for bar in bars:
        if not ui.visible(bar):
            continue
        result = ui.call("get_element_tree", elementHandle=bar, maxElements=1000)
        assert not result["truncated"]
        labels = [item.get("accessibleLabel") for item in result["elements"]]
        if title in labels and "暂停" in labels:
            return time.time()
    return None


def open_reasons(client, entry):
    if entry == "drawer":
        client.ui.playback_options()
        activate(client.ui, "不喜欢")
    else:
        daily(client.ui)
        long_press(client)
        button(client.ui, "屏蔽标签")
        activate(client.ui, "不喜欢…")
    for label in REASONS.values():
        handle = button(client.ui, label)
        properties = client.ui.call("get_element_properties", elementHandle=handle)
        assert properties["size"]["height"] >= 44
        assert properties["size"]["width"] >= 44


def rules(world):
    return world.sql("SELECT * FROM block_rules WHERE account_id=%s", (world.account,))


def feedback(world):
    return world.sql(
        "SELECT track_id, verdict FROM track_feedback WHERE account_id=%s", (world.account,)
    )


def current_group_track(world):
    group = world.group()
    if not group or group["entry_id"] is None:
        return None
    entries = world.sql(
        "SELECT track_id FROM play_queue_entries WHERE queue_id=%s AND revision=%s AND entry_id=%s",
        (group["queue_id"], group["revision"], group["entry_id"]),
    )
    return entries[0]["track_id"] if entries else None


# 读取真实服务端的 ready 提前记录，不伪造执行报告或锚点。
def ready_report_time(world, entry_id):
    for log in world.directory.glob("server-*.log"):
        for raw in log.read_text().splitlines():
            line = re.sub(r"\x1b\[[0-9;]*m", "", raw)
            if "出声设备都就绪,起播提前" in line and re.search(
                rf"\bentry_id={entry_id}\b", line
            ):
                return datetime.fromisoformat(line.split()[0]).timestamp()
    return None


def published_local_tracks(world):
    return world.sql(
        """WITH current_queue AS (
            SELECT q.id,r.applied_revision FROM play_queues q
            JOIN play_queue_reports r ON r.queue_id=q.id
            WHERE q.account_id=%s AND q.device_id=%s
            ORDER BY r.reported_at DESC LIMIT 1
        ) SELECT e.track_id FROM current_queue q JOIN play_queue_entries e
        ON e.queue_id=q.id AND e.revision=q.applied_revision ORDER BY e.position""",
        (world.account, world.controller.device()),
    )


def choose(world, reason):
    activate(world.controller.ui, REASONS[reason])
    if reason == "artist":
        assert not rules(world), "multiple artists must not silently choose the first artist"
        activate(world.controller.ui, "Artist A")
    saved = wait_until(lambda: rules(world), "saved dislike rule")
    assert len(saved) == 1
    expected = (
        "artist" if reason == "artist" else "song_versions" if reason == "versions" else "song"
    )
    assert saved[0]["kind"] == expected
    expected_feedback = [{"track_id": "175001", "verdict": -1}] if reason == "exact" else []
    assert feedback(world) == expected_feedback
    label = "Artist A" if reason == "artist" else "Song — Artist A / Artist B"
    if reason == "versions":
        label += "（含其他版本）"
    assert saved[0]["label"] == label
    (world.directory / "dislike-saved.json").write_text(
        json.dumps({"rules": saved, "feedback": feedback(world)}, default=str, indent=2)
    )
    return saved[0]


# 当前批、未来重载以及设置页撤销分别观察，不以一次过滤替代全部结果。
def assert_filtered(world, reason):
    ui = world.controller.ui
    daily(ui)
    expected = [CATALOG[key][0] for key in CATALOG if key not in BLOCKED[reason]]
    wait_until(lambda: rows(ui) == expected, "daily applies saved rule")
    assert rows(ui) == expected
    activate(ui, "展开播放页")
    ui.activate(ui.must("PlayPage::queue-entry-touch"), pointer=True)

    def visible_queue():
        queues = ui.call(
            "query_element_descendants",
            elementHandle=ui.root,
            findAll=True,
            queryStack=[{"matchElementTypeName": "QueuePage"}],
        ).get("elementHandles", [])
        visible = [queue for queue in queues if ui.visible(queue)]
        return visible[0] if len(visible) == 1 else None

    queue = wait_until(visible_queue, "visible queue page")
    wait_until(lambda: rows(ui, queue) == expected, "queue applies saved rule")
    assert rows(ui, queue) == expected, "existing queue must hide every matching entry"
    ui.call("dispatch_key_event", windowHandle=ui.window, text="\x1b")


# 两入口和三理由全部贯通；SQL 是规则/反馈真相源，PCM 是出声真相源。
@pytest.mark.parametrize("entry", ("drawer", "list"))
@pytest.mark.parametrize("reason", ("artist", "versions", "exact"))
def test_dislike_saves_skips_and_filters(dislike_world, entry, reason):
    world = dislike_world
    source = world.controller
    source.ui.pick("list")
    world.sound(source, "175001", group=False)
    open_reasons(source, entry)
    saved = choose(world, reason)
    wanted = NEXT[reason]
    switched = wait_until(
        lambda: playing_title(source.ui, CATALOG[wanted][0]),
        "next song UI after saved rule",
        timeout=1,
    )
    elapsed = switched - saved["created_at"].timestamp()
    (world.directory / "dislike-latency.json").write_text(
        json.dumps(
            {
                "saved_at": saved["created_at"].timestamp(),
                "observed_at": switched,
                "seconds": elapsed,
            }
        )
    )
    assert 0 <= elapsed <= 1, "saved dislike must switch the real playback entry within one second"
    world.sound(source, wanted, group=False)
    assert_filtered(world, reason)
    expected_ids = [{"track_id": key} for key in CATALOG if key not in BLOCKED[reason]]
    wait_until(
        lambda: published_local_tracks(world) == expected_ids,
        "physical local queue publication removes blocked tracks",
    )


# 远端出声者不靠及时刷新 BlockSet；真实下一首与随后服务端推进都跳过匹配条目。
def test_dislike_remote_output_and_future_advance(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.output(world.one)
    source.ui.pick("list")
    world.sound(world.one, "175001")
    open_reasons(source, "drawer")
    saved = choose(world, "exact")
    wait_until(lambda: current_group_track(world) == "175003", "group skips duplicate", timeout=1)
    switched = wait_until(
        lambda: playing_title(source.ui, "Song (Live)"), "group playback bar updates", timeout=1
    )
    saved_at = saved["created_at"].timestamp()
    assert 0 <= switched - saved_at <= 1
    group = world.group()
    initial_anchor = group["anchor_wall_us"] / 1_000_000
    # 既有协议保留3秒预缓冲；250ms只覆盖保存事务到组意图的往返余量。
    assert 0 <= initial_anchor - saved_at <= 3.25
    world.sound(world.one, "175003")
    ready_at = wait_until(
        lambda: ready_report_time(world, group["entry_id"]), "actual output ready report"
    )
    anchor = world.group()["anchor_wall_us"] / 1_000_000
    assert 0.4 <= anchor - ready_at <= 0.6, "ready advances anchor to report time + 500ms"
    assert anchor <= initial_anchor
    assert anchor < saved_at + 3, "ready output must advance the original start wait"
    world.silent(source, world.two)
    assert_filtered(world, "exact")
    daily(source.ui)
    source.ui.activate(source.ui.must("TrackList::touch", "Other"), action="Expand")
    activate(source.ui, "不喜欢…")
    activate(source.ui, REASONS["exact"])
    wait_until(lambda: len(rules(world)) == 2, "future group track blocked")
    assert current_group_track(world) == "175003", "blocking a future entry must keep current audio"
    world.sound(world.one, "175003")
    source.ui.seek(88)
    world.sound(world.one, "175005")
    assert current_group_track(world) == "175005", (
        "automatic group advance must skip blocked 175004"
    )
    world.silent(source, world.two)


# 启动重新拉规则，设置页恢复重载目录。
def test_dislike_restart_persists_and_settings_unblock_restores(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.pick("list")
    world.sound(source, "175001", group=False)
    open_reasons(source, "drawer")
    saved = choose(world, "versions")
    world.sound(source, "175004", group=False)
    source.restart()
    daily(source.ui)
    wait_until(lambda: rows(source.ui) == ["Other", "Song", "Safe"], "restart loads saved rules")
    assert rows(source.ui) == ["Other", "Song", "Safe"]
    activate(source.ui, "设置")
    activate(source.ui, "恢复 " + saved["label"])
    wait_until(lambda: not rules(world), "unblocked rule deleted")
    daily(source.ui)
    wait_until(
        lambda: rows(source.ui) == [value[0] for value in CATALOG.values()],
        "unblock reload completes",
    )
    assert rows(source.ui) == [value[0] for value in CATALOG.values()]


# 放弃理由/多歌手选择都不得偷偷保存；现有赞保留为反馈，不新增规则。
def test_dislike_cancel_and_existing_praise(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.pick("list")
    world.sound(source, "175001", group=False)
    open_reasons(source, "drawer")
    # 遮罩有明确 id，点在远离中央菜单的窗口空白处。
    cancel(source)
    assert not rules(world) and not feedback(world)
    open_reasons(source, "drawer")
    activate(source.ui, REASONS["artist"])
    assert not rules(world)
    cancel(source)
    assert not rules(world) and not feedback(world)
    source.ui.playback_options()
    activate(source.ui, "点赞")
    wait_until(lambda: feedback(world), "existing praise saved")
    assert feedback(world) == [{"track_id": "175001", "verdict": 1}]
    assert not rules(world)
    world.sound(source, "175001", group=False)


# 电台已有批次和刷新都隐藏匹配项，不能只重载日推或者只跳过 id。
def test_dislike_radio_batch_and_future_fetch(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.radio()
    world.sound(source, "175001", group=False)
    open_reasons(source, "drawer")
    choose(world, "exact")
    world.sound(source, "175003", group=False)
    source.ui.music("radio")
    source.ui.activate(source.ui.must("WallView::view-list-btn"))
    expected = ["Song (Live)", "Other", "Song", "Safe"]
    wait_until(lambda: rows(source.ui) == expected, "radio applies saved rule")
    assert rows(source.ui) == expected
    source.restart()
    source.ui.radio()
    source.ui.activate(source.ui.must("WallView::view-list-btn"))
    wait_until(lambda: rows(source.ui), "new radio batch")
    wait_until(lambda: rows(source.ui) == expected, "radio applies saved rule")
    assert rows(source.ui) == expected
    world.sound(source, "175003", group=False)


# 写入失败必须回滚点踩且不移除/跳过当前曲；故障只存在本用例的私有库。
def test_dislike_failed_save_keeps_playback_and_feedback(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.pick("list")
    world.sound(source, "175001", group=False)
    world.sql("ALTER TABLE block_rules ADD CONSTRAINT reject_dislike CHECK (false) NOT VALID")
    open_reasons(source, "drawer")
    activate(source.ui, REASONS["exact"])
    wait_until(lambda: source.ui.elements("MainWindow::banner"), "save failure shown")
    assert not rules(world) and not feedback(world), "failed rule must not leave partial feedback"
    assert playing_title(source.ui, "Song")
    world.sound(source, "175001", group=False)


# 真正选择第二位歌手；只含第一位的重复歌和版本仍可播放。
def test_dislike_artist_picker_uses_selected_artist(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.pick("list")
    world.sound(source, "175001", group=False)
    open_reasons(source, "drawer")
    activate(source.ui, REASONS["artist"])
    assert not rules(world)
    activate(source.ui, "Artist B")
    saved = wait_until(lambda: rules(world), "selected artist saved")
    assert len(saved) == 1 and saved[0]["label"] == "Artist B"
    assert not feedback(world)
    world.sound(source, "175002", group=False)
    daily(source.ui)
    expected = [CATALOG[key][0] for key in CATALOG if key != "175001"]
    wait_until(lambda: rows(source.ui) == expected, "selected artist or legacy rule filtered")
    assert rows(source.ui) == expected


# 老账号的按 id 规则继续过滤和展示，恢复时不误删其他规则。
def test_dislike_legacy_id_rule_can_be_restored(dislike_world):
    world = dislike_world
    world.sql(
        "INSERT INTO block_rules (account_id,kind,value,label) VALUES (%s,'track','175001','Legacy Song')",
        (world.account,),
    )
    source = world.controller
    source.restart()
    daily(source.ui)
    expected = [CATALOG[key][0] for key in CATALOG if key != "175001"]
    wait_until(lambda: rows(source.ui) == expected, "selected artist or legacy rule filtered")
    assert rows(source.ui) == expected
    activate(source.ui, "设置")
    activate(source.ui, "恢复 Legacy Song")
    wait_until(lambda: not rules(world), "legacy rule restored")
    daily(source.ui)
    wait_until(
        lambda: rows(source.ui) == [value[0] for value in CATALOG.values()],
        "unblock reload completes",
    )
    assert rows(source.ui) == [value[0] for value in CATALOG.values()]


# 屏蔽非当前曲目不能跳歌；删掉中间条目后，下一首仍是当前曲目的原后继。
def test_dislike_noncurrent_track_preserves_cursor(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.pick("list")
    world.sound(source, "175001", group=False)
    daily(source.ui)
    source.ui.activate(source.ui.must("TrackList::touch", "Other"), action="Expand")
    activate(source.ui, "不喜欢…")
    activate(source.ui, REASONS["exact"])
    saved = wait_until(lambda: rules(world), "noncurrent song saved")
    assert len(saved) == 1 and saved[0]["label"] == "Other — Artist A"
    assert feedback(world) == [{"track_id": "175004", "verdict": -1}]
    assert playing_title(source.ui, "Song")
    world.sound(source, "175001", group=False)
    expected_ids = [{"track_id": key} for key in CATALOG if key != "175004"]
    wait_until(lambda: published_local_tracks(world) == expected_ids, "remove noncurrent track")
    source.ui.transport("next")
    world.sound(source, "175002", group=False)


# 旧列表响应必须可按固定旧枚举解析，新客户端声明能力后才能管理新规则。
def test_dislike_rule_capability_preserves_old_client_response(dislike_world):
    world = dislike_world
    source = world.controller
    source.ui.pick("list")
    world.sound(source, "175001", group=False)
    open_reasons(source, "drawer")
    saved = choose(world, "exact")
    world.sql(
        "INSERT INTO block_rules (account_id, kind, value, label) VALUES (%s, 'track', %s, %s)",
        (world.account, "175006", "Safe"),
    )
    artist_value = json.dumps({"platform": "netease", "id": "9", "name": "Artist C"})
    world.sql(
        "INSERT INTO block_rules (account_id, kind, value, label) VALUES (%s, 'artist', %s, %s)",
        (world.account, artist_value, "Artist C"),
    )
    legacy = read_blocks(world, "/blocks")
    assert [LegacyBlockKind(rule["kind"]) for rule in legacy] == [
        LegacyBlockKind.TRACK,
        LegacyBlockKind.ARTIST,
    ]
    assert legacy[1]["value"] == "Artist C", "old clients match artist rules by literal name"
    assert legacy[0]["value"] == "175006"
    current = read_blocks(world, "/blocks?song_rules=true")
    assert {rule["kind"] for rule in current} == {"song", "track", "artist"}
    with pytest.raises(ValueError):
        [LegacyBlockKind(rule["kind"]) for rule in current]
    # 启动刷新不能漏掉能力声明，否则设置页无法找到已保存的新规则。
    source.restart()
    daily(source.ui)
    wait_until(lambda: rows(source.ui) == ["Song (Live)", "Other"], "all rules filtered")
    activate(source.ui, "设置")
    button(source.ui, "恢复 " + saved["label"])
