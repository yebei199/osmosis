//! 服务端持久播放队列的集成测试(`docs/adr/0031`)。
//!
//! 打真库:这一层要说的话("重复项不合并"、"旧 epoch 的报告被拒"、"清缓存
//! 不动队列")全都是数据库的行为,换成内存假货就等于测了另一个东西。
//!
//! 每条测试用完即回滚,与 `playlists.rs` 同一个套路。

use server::store::account::{Account, register};
use server::store::db;
use server::store::queue::{self, EntryInput, Report};
use sqlx::{PgPool, Postgres, Transaction};

/// 与 `main.rs` 的默认值一致。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

const INVITE: &str = "let-me-in";

/// 这一台的设备 id。队列归属于播放会话 / 输出设备,不是账号。
const PC1: &str = "pc1";

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(
        |_| DEFAULT_DATABASE_URL.to_owned(),
    );

    db::connect(&url).await.unwrap_or_else(|err| {
        panic!(
            "连不上数据库({url}): {err}\n\
             起一个:just pg"
        )
    })
}

async fn tx() -> Transaction<'static, Postgres> {
    pool().await.begin().await.expect("开事务失败")
}

async fn make_account(
    tx: &mut Transaction<'static, Postgres>,
    username: &str,
) -> Account {
    register(tx, username, "correct horse", INVITE, INVITE)
        .await
        .expect("注册应该成功")
}

/// 一条上传的条目。展示信息带着走 —— 队列条目不向 `platform_tracks` 要详情。
fn entry(id: &str) -> EntryInput {
    EntryInput {
        platform: "netease".to_owned(),
        track_id: id.to_owned(),
        title: format!("歌 {id}"),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: 234_000,
    }
}

/// 一份最小的执行报告。
fn report(
    epoch: i64,
    state_seq: i64,
    applied_revision: i64,
) -> Report {
    Report {
        device_id: PC1.to_owned(),
        epoch,
        state_seq,
        applied_revision,
        entry_id: None,
        play_order: Vec::new(),
        round: 0,
        position_ms: 0,
        play_state: "playing".to_owned(),
    }
}

/// 建一个队列拿到 `queue_id` 与第一个版本,读回来顺序与上传时一致。
#[tokio::test]
async fn publishing_a_queue_reads_back_in_the_uploaded_order()
 {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_create").await;

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b"), entry("c")],
    )
    .await
    .expect("发布队列应该成功");

    let page = queue::page(
        &mut tx,
        account.id,
        published.queue_id,
        published.revision,
        0,
        100,
    )
    .await
    .expect("读队列应该成功");

    assert_eq!(published.revision, 1);
    let ids: Vec<&str> = page
        .iter()
        .map(|row| row.track_id.as_str())
        .collect();
    assert_eq!(ids, vec!["a", "b", "c"]);
    assert_eq!(page[0].title, "歌 a");
}

/// 同一首歌出现两次就是两个条目,`entry_id` 各不相同(AC-4)。
///
/// 歌单那两张表的主键是「同曲唯一」,复用它的话这里会静默合并成一条 ——
/// 而用户明明在队列里放了两次。
#[tokio::test]
async fn the_same_track_twice_stays_two_entries() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_dup").await;

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b"), entry("a")],
    )
    .await
    .expect("发布队列应该成功");

    let page = queue::page(
        &mut tx,
        account.id,
        published.queue_id,
        published.revision,
        0,
        100,
    )
    .await
    .expect("读队列应该成功");

    assert_eq!(page.len(), 3);
    assert_eq!(page[0].track_id, "a");
    assert_eq!(page[2].track_id, "a");
    assert_ne!(
        page[0].entry_id, page[2].entry_id,
        "同一首歌的两次出现该是两个条目"
    );
}

/// 改一次队列产生新版本,活下来的条目**沿用原来的 `entry_id`**。
///
/// 不稳定的话,播放端手上那个 `(applied_revision, entry_id)` 与服务端的新版本
/// 根本对不上账 —— 而对账正是持久化新增复杂度的全部所在(`docs/adr/0031` 二)。
#[tokio::test]
async fn republishing_keeps_entry_ids_of_surviving_tracks()
{
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_stable").await;

    let first = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b")],
    )
    .await
    .expect("发布队列应该成功");
    let before = queue::page(
        &mut tx,
        account.id,
        first.queue_id,
        first.revision,
        0,
        100,
    )
    .await
    .expect("读队列应该成功");

    // 前面插一首、顺序因此整体后挪一位。
    let second = queue::publish(
        &mut tx,
        account.id,
        first.queue_id,
        first.revision,
        &[entry("z"), entry("a"), entry("b")],
    )
    .await
    .expect("改队列应该成功");
    let after = queue::page(
        &mut tx,
        account.id,
        second.queue_id,
        second.revision,
        0,
        100,
    )
    .await
    .expect("读队列应该成功");

    assert_eq!(second.revision, 2);
    assert_eq!(after[1].entry_id, before[0].entry_id);
    assert_eq!(after[2].entry_id, before[1].entry_id);
    assert!(
        after[0].entry_id > before[1].entry_id,
        "新插进来的那首该拿一个新号"
    );
}

/// 一次读取固定在一个版本上:新版本发布之后,旧版本读出来仍是旧顺序。
///
/// 不钉死的话,分页读一份长队列会前半页旧顺序、后半页新顺序,
/// 而那种错在界面上看起来只是"有几首歌重复了"。
#[tokio::test]
async fn a_read_is_pinned_to_the_revision_it_asked_for() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_pinned").await;

    let first = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b")],
    )
    .await
    .expect("发布队列应该成功");
    queue::publish(
        &mut tx,
        account.id,
        first.queue_id,
        first.revision,
        &[entry("b"), entry("a")],
    )
    .await
    .expect("改队列应该成功");

    let old = queue::page(
        &mut tx,
        account.id,
        first.queue_id,
        first.revision,
        0,
        100,
    )
    .await
    .expect("旧版本该还读得到");

    let ids: Vec<&str> = old
        .iter()
        .map(|row| row.track_id.as_str())
        .collect();
    assert_eq!(ids, vec!["a", "b"]);
}

/// 拿一个过期的 `expected_revision` 去改,整次改动被拒(AC-8)。
///
/// 两台设备同时改同一个队列时,后到的那次不能凭"我也有一份完整列表"
/// 把别人的新版本盖掉。
#[tokio::test]
async fn publishing_against_a_stale_revision_is_refused() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_stale").await;

    let first = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a")],
    )
    .await
    .expect("发布队列应该成功");
    queue::publish(
        &mut tx,
        account.id,
        first.queue_id,
        first.revision,
        &[entry("a"), entry("b")],
    )
    .await
    .expect("第一次改应该成功");

    let refused = queue::publish(
        &mut tx,
        account.id,
        first.queue_id,
        first.revision,
        &[entry("c")],
    )
    .await;

    assert!(
        refused.is_err(),
        "拿旧版本号去改该被拒,得到的是 {refused:?}"
    );
}

/// 别人的队列读不到。归属检查写进 WHERE,不是单独一步。
#[tokio::test]
async fn a_queue_is_scoped_to_its_account() {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "q_scope_a").await;
    let other = make_account(&mut tx, "q_scope_b").await;

    let published =
        queue::create(&mut tx, mine.id, PC1, &[entry("a")])
            .await
            .expect("发布队列应该成功");

    let stolen = queue::page(
        &mut tx,
        other.id,
        published.queue_id,
        published.revision,
        0,
        100,
    )
    .await;

    assert!(stolen.is_err(), "别的账号不该读得到这个队列");
}

/// 超出约定规模**明确拒绝**,不截断(AC-6)。
#[tokio::test]
async fn an_oversized_upload_is_refused_not_truncated() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_quota").await;

    let too_many: Vec<EntryInput> = (0
        ..contract::MAX_QUEUE_ENTRIES + 1)
        .map(|index| entry(&index.to_string()))
        .collect();

    let refused =
        queue::create(&mut tx, account.id, PC1, &too_many)
            .await;

    assert!(
        refused.is_err(),
        "超限该被拒,得到的是 {refused:?}"
    );
}

/// 清掉平台曲目缓存,队列条目、顺序与重复项一个不动(AC-4)。
///
/// 队列条目不外键指向 `platform_tracks`。挂上 CASCADE 的话,一次清缓存
/// 会把用户攒的队列一起删掉;挂 RESTRICT 又把缓存钉死成不可删。
#[tokio::test]
async fn clearing_the_platform_cache_leaves_the_queue_intact()
 {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_cache").await;

    sqlx::query(
        "INSERT INTO platform_tracks
             (platform, track_id, title, artists, duration_ms)
         VALUES ('netease', 'a', '歌 a', ARRAY['LiSA'], 234000)",
    )
    .execute(&mut *tx)
    .await
    .expect("写缓存应该成功");

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b"), entry("a")],
    )
    .await
    .expect("发布队列应该成功");

    sqlx::query("DELETE FROM platform_tracks")
        .execute(&mut *tx)
        .await
        .expect("清缓存应该成功");

    let page = queue::page(
        &mut tx,
        account.id,
        published.queue_id,
        published.revision,
        0,
        100,
    )
    .await
    .expect("清缓存之后队列该还在");

    let ids: Vec<&str> = page
        .iter()
        .map(|row| row.track_id.as_str())
        .collect();
    assert_eq!(ids, vec!["a", "b", "a"]);
}

/// 已提交的待应用意图存得住:WebSocket 那条通知丢了也不会永久失去它(AC-8)。
#[tokio::test]
async fn a_pending_intent_survives_to_be_read_back() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_intent").await;

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b")],
    )
    .await
    .expect("发布队列应该成功");
    let entries = queue::page(
        &mut tx,
        account.id,
        published.queue_id,
        published.revision,
        0,
        100,
    )
    .await
    .expect("读队列应该成功");

    queue::set_intent(
        &mut tx,
        account.id,
        published.queue_id,
        PC1,
        published.revision,
        entries[1].entry_id,
        "op-1",
    )
    .await
    .expect("写意图应该成功");

    let head = queue::head(
        &mut tx,
        account.id,
        published.queue_id,
    )
    .await
    .expect("读队列头应该成功");
    let intent = head.intent.expect("该有一条待应用的意图");

    assert_eq!(intent.operation_id, "op-1");
    assert_eq!(intent.entry_id, entries[1].entry_id);
    assert_eq!(intent.state, "pending");
    assert_eq!(head.revision, published.revision);
    assert_eq!(head.total, 2);
}

/// 连点 A、B:后到的 B 成为待应用意图,迟到的 A 应用不上(AC-5)。
#[tokio::test]
async fn the_later_intent_replaces_the_earlier_one() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_ab").await;

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b")],
    )
    .await
    .expect("发布队列应该成功");
    let entries = queue::page(
        &mut tx,
        account.id,
        published.queue_id,
        published.revision,
        0,
        100,
    )
    .await
    .expect("读队列应该成功");

    for (index, operation) in [(0, "op-a"), (1, "op-b")] {
        queue::set_intent(
            &mut tx,
            account.id,
            published.queue_id,
            PC1,
            published.revision,
            entries[index].entry_id,
            operation,
        )
        .await
        .expect("写意图应该成功");
    }

    let head = queue::head(
        &mut tx,
        account.id,
        published.queue_id,
    )
    .await
    .expect("读队列头应该成功");
    let intent = head.intent.expect("该有一条待应用的意图");

    assert_eq!(intent.operation_id, "op-b");
    assert_eq!(intent.entry_id, entries[1].entry_id);
}

/// 旧 epoch 的迟到报告被拒,库里留着的仍是新的那一份(AC-8)。
///
/// 播放端重启换一个 epoch。上一条连接上飘过来的残余报告不能把新进程的状态盖掉。
#[tokio::test]
async fn a_report_from_an_older_epoch_is_refused() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_epoch").await;

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a")],
    )
    .await
    .expect("发布队列应该成功");

    let accepted = queue::record_report(
        &mut tx,
        account.id,
        published.queue_id,
        &report(200, 1, 1),
    )
    .await
    .expect("写报告应该成功");
    let stale = queue::record_report(
        &mut tx,
        account.id,
        published.queue_id,
        &report(100, 99, 1),
    )
    .await
    .expect("写报告应该成功");

    let head = queue::head(
        &mut tx,
        account.id,
        published.queue_id,
    )
    .await
    .expect("读队列头应该成功");

    assert!(accepted, "新 epoch 的报告该收下");
    assert!(!stale, "旧 epoch 的报告该被拒");
    assert_eq!(
        head.report.expect("该有一份报告").epoch,
        200
    );
}

/// 同一个 epoch 里序号倒退的报告同样被拒。
#[tokio::test]
async fn a_report_out_of_sequence_is_refused() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_seq").await;

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a")],
    )
    .await
    .expect("发布队列应该成功");

    queue::record_report(
        &mut tx,
        account.id,
        published.queue_id,
        &report(200, 5, 1),
    )
    .await
    .expect("写报告应该成功");
    let stale = queue::record_report(
        &mut tx,
        account.id,
        published.queue_id,
        &report(200, 4, 1),
    )
    .await
    .expect("写报告应该成功");

    let head = queue::head(
        &mut tx,
        account.id,
        published.queue_id,
    )
    .await
    .expect("读队列头应该成功");

    assert!(!stale, "序号倒退的报告该被拒");
    assert_eq!(
        head.report.expect("该有一份报告").state_seq,
        5
    );
}

/// 播放端还停在旧版本上时,那个版本不能被回收掉(`docs/adr/0031` 五)。
///
/// 回收掉的话,断连回来的播放端既读不到自己手上那一版,也没法与新版本对账 ——
/// 而它此刻正在照着那一版继续放。
#[tokio::test]
async fn the_revision_a_player_still_holds_is_not_reclaimed()
 {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_retain").await;

    let first = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a")],
    )
    .await
    .expect("发布队列应该成功");
    queue::record_report(
        &mut tx,
        account.id,
        first.queue_id,
        &report(200, 1, first.revision),
    )
    .await
    .expect("写报告应该成功");

    // 连改五次,足够把任何"只留最近几版"的策略推过头。
    let mut latest = first.revision;
    for round in 0..5 {
        latest = queue::publish(
            &mut tx,
            account.id,
            first.queue_id,
            latest,
            &[entry("a"), entry(&round.to_string())],
        )
        .await
        .expect("改队列应该成功")
        .revision;
    }

    let held = queue::page(
        &mut tx,
        account.id,
        first.queue_id,
        first.revision,
        0,
        100,
    )
    .await
    .expect("播放端手上那一版该还读得到");

    assert_eq!(held.len(), 1);
    assert_eq!(held[0].track_id, "a");
}
