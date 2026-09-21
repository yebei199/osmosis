//! 服务端持久播放队列的集成测试(`docs/adr/0031`)。
//!
//! 打真库:这一层要说的话("重复项不合并"、"旧 epoch 的报告被拒"、"清缓存
//! 不动队列")全都是数据库的行为,换成内存假货就等于测了另一个东西。
//!
//! 每条测试用完即回滚,与 `playlists.rs` 同一个套路。

use server::error::AppError;
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
        play_order: None,
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
    .expect("读队列应该成功")
    .entries;

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
    .expect("读队列应该成功")
    .entries;

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
    .expect("读队列应该成功")
    .entries;

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
    .expect("读队列应该成功")
    .entries;

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
    .expect("旧版本该还读得到")
    .entries;

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
    .expect("清缓存之后队列该还在")
    .entries;

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
    .expect("读队列应该成功")
    .entries;

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
    .expect("读队列应该成功")
    .entries;

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

/// 不带排列的报告**沿用库里那份**,不会把它清空。
///
/// 每秒那条上报走的就是这条路:排列一个小时也不变一次,而每秒把它一起写回去
/// 就是每秒重写五千个 bigint。线上字节数不涨(AC-2 照过),写放大全落在库里 ——
/// 这是个 AC-2 抓不到的洞,所以单独钉住。
#[tokio::test]
async fn a_report_without_an_order_keeps_the_stored_one() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_order").await;

    let published = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a")],
    )
    .await
    .expect("发布队列应该成功");

    // 洗了一次牌:这一条带着排列。
    let mut shuffled = report(200, 1, published.revision);
    shuffled.play_order = Some(vec![3, 1, 2]);
    queue::record_report(
        &mut tx,
        account.id,
        published.queue_id,
        &shuffled,
    )
    .await
    .expect("写报告应该成功");

    // 随后每秒那条:只报位置,不带排列。
    let mut ticking = report(200, 2, published.revision);
    ticking.position_ms = 1_000;
    queue::record_report(
        &mut tx,
        account.id,
        published.queue_id,
        &ticking,
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
    let stored = head.report.expect("该有一份报告");

    assert_eq!(
        stored.play_order,
        Some(vec![3, 1, 2]),
        "不带排列的报告不该把库里那份清掉"
    );
    assert_eq!(stored.position_ms, 1_000);
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
    .expect("播放端手上那一版该还读得到")
    .entries;

    assert_eq!(held.len(), 1);
    assert_eq!(held[0].track_id, "a");
}

/// 建到上限就**明确拒绝**,而且与「这一批太长」「这一阵太密」分开(F-003)。
///
/// 三种拒绝的出路完全不同:太长换个短的、太密等一等、到顶了没得等。
/// 混成一个 code 的话,客户端会对着一个永远不会好的拒绝无限退避重试。
#[tokio::test]
async fn creating_past_the_account_quota_is_refused() {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_quota_n").await;

    for round in 0..queue::MAX_QUEUES_PER_ACCOUNT {
        queue::create(
            &mut tx,
            account.id,
            &format!("dev-{round}"),
            &[entry("a")],
        )
        .await
        .expect("上限之内该建得出来");
    }

    let refused = queue::create(
        &mut tx,
        account.id,
        "dev-over",
        &[entry("a")],
    )
    .await;

    assert!(
        matches!(
            refused,
            Err(AppError::QueueQuotaExceeded)
        ),
        "到上限该回 QueueQuotaExceeded,得到的是 {refused:?}"
    );
}

/// 配额是**按账号**算的:别人的队列不占我的额度。
#[tokio::test]
async fn the_quota_is_counted_per_account() {
    let mut tx = tx().await;
    let mine = make_account(&mut tx, "q_quota_mine").await;
    let other =
        make_account(&mut tx, "q_quota_other").await;

    for round in 0..queue::MAX_QUEUES_PER_ACCOUNT {
        queue::create(
            &mut tx,
            other.id,
            &format!("dev-{round}"),
            &[entry("a")],
        )
        .await
        .expect("别人建满自己的额度");
    }

    queue::create(&mut tx, mine.id, PC1, &[entry("a")])
        .await
        .expect("别人满了不该占我的额度");
}

/// 惰性清理只收**没人认领过**的那些,有执行报告的一个都不碰。
///
/// 这一条是 F-003 第七点的正身:**别拿 TTL 猜**。没上报不等于孤儿 ——
/// 设备可能只是离线,而它一回来就要接着放。误删的话,用户下次开机发现
/// 自己的队列没了,而日志里什么都看不出来。
#[tokio::test]
async fn cleanup_spares_queues_a_player_has_claimed() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "q_quota_spare").await;

    let claimed = queue::create(
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
        claimed.queue_id,
        &report(200, 1, claimed.revision),
    )
    .await
    .expect("写报告应该成功");

    // 把它推到宽限期之外,让清理有机会看见它。
    sqlx::query(
        "UPDATE play_queues SET updated_at = now() - INTERVAL '2 days'
         WHERE id = $1",
    )
    .bind(claimed.queue_id)
    .execute(&mut *tx)
    .await
    .expect("改时间应该成功");

    // 再建一个,顺带触发那次惰性清理。
    queue::create(
        &mut tx,
        account.id,
        "dev-2",
        &[entry("b")],
    )
    .await
    .expect("发布队列应该成功");

    let head =
        queue::head(&mut tx, account.id, claimed.queue_id)
            .await;
    assert!(
        head.is_ok(),
        "有执行报告的队列不该被当成孤儿收走"
    );
}

/// 反过来:从没人认领、又过了宽限期的,该收就收。
#[tokio::test]
async fn cleanup_collects_queues_nobody_ever_claimed() {
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "q_quota_orphan").await;

    let orphan = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a")],
    )
    .await
    .expect("发布队列应该成功");
    sqlx::query(
        "UPDATE play_queues SET updated_at = now() - INTERVAL '2 days'
         WHERE id = $1",
    )
    .bind(orphan.queue_id)
    .execute(&mut *tx)
    .await
    .expect("改时间应该成功");

    queue::create(
        &mut tx,
        account.id,
        "dev-2",
        &[entry("b")],
    )
    .await
    .expect("发布队列应该成功");

    let head =
        queue::head(&mut tx, account.id, orphan.queue_id)
            .await;
    assert!(
        matches!(head, Err(AppError::NotFound)),
        "没人认领又过期的该被收走,得到的是 {head:?}"
    );
}

/// 这个账号名下还剩几条队列。
async fn count_queues(
    tx: &mut Transaction<'static, Postgres>,
    account_id: i64,
) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM play_queues WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_one(&mut **tx)
    .await
    .expect("数队列应该成功");
    n
}

/// 应用重启之后再点歌,**不该**再建一个队列。
///
/// 客户端那句「已经有这台设备的队列就发新版本」靠的是内存里的 `queue_id`,
/// 进程一退就没了,而**没有任何路由能按设备找回它**(`head` 要你已经有 id)。
/// 于是每重启一次就多一条;而放过歌的队列有报告,`reclaim_orphans` 明确
/// 一个都不碰 —— 重启到第十次,账号就永久卡在配额上,客户端侧没有出路。
/// 2026-09-21 线上就是这么卡死的:探 `/queues/{id}/head`,id 1–10 全归本账号。
///
/// 所以这个不变量只能由服务端保证,不能指望客户端记着:
/// **一台设备一条当前队列**(`docs/adr/0031` 二)。
#[tokio::test]
async fn a_restarted_device_reuses_its_queue_instead_of_burning_a_slot()
 {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_reuse").await;

    let first = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a")],
    )
    .await
    .expect("第一次建队列应该成功");
    // 放过歌 —— 有报告的队列惰性清理永远不收,正是它让槽位漏掉。
    queue::record_report(
        &mut tx,
        account.id,
        first.queue_id,
        &report(200, 1, first.revision),
    )
    .await
    .expect("写报告应该成功");

    // 重启:客户端丢了 queue_id,于是又来建一次。
    let again = queue::create(
        &mut tx,
        account.id,
        PC1,
        &[entry("a"), entry("b")],
    )
    .await
    .expect("重启后再点歌不该被拒");

    assert_eq!(
        again.queue_id, first.queue_id,
        "同一台设备该重用自己那条队列,而不是再建一条"
    );
    assert!(
        again.revision > first.revision,
        "重用要发一个新版本,得到的是 {} → {}",
        first.revision,
        again.revision
    );
    assert_eq!(
        count_queues(&mut tx, account.id).await,
        1,
        "同一台设备不该多出一条队列"
    );
}

/// 一台设备反复重启,烧不掉这个账号的配额。
///
/// 上一条测的是「重用」,这一条测的是它的后果:重启次数超过上限也不该被拒。
#[tokio::test]
async fn one_device_cannot_exhaust_the_quota_by_restarting()
{
    let mut tx = tx().await;
    let account =
        make_account(&mut tx, "q_reuse_quota").await;

    for round in 0..queue::MAX_QUEUES_PER_ACCOUNT + 5 {
        queue::create(
            &mut tx,
            account.id,
            PC1,
            &[entry(&round.to_string())],
        )
        .await
        .unwrap_or_else(|err| {
            panic!("第 {round} 次重启就被拒了: {err:?}")
        });
    }

    assert_eq!(
        count_queues(&mut tx, account.id).await,
        1,
        "十五次重启之后仍然只该有一条队列"
    );
}

/// 卡死的账号,新设备一来就该自愈 —— 不必等别的设备先各点一次歌。
///
/// 这一条测的是修复的**回收面**:线上那十条残留分属手机和 pc1,而平板是
/// 一台全新的设备。只收「调用方自己那台」的话,平板第一次建队列照样撞满,
/// 得等手机和 pc1 各来一趟才有余量 —— 那个先后顺序用户看不见也控制不了。
#[tokio::test]
async fn a_brand_new_device_heals_an_account_stuck_at_the_quota()
 {
    let mut tx = tx().await;
    let account = make_account(&mut tx, "q_heal").await;

    // 造出修复之前的现场:两台设备各漏了五条,而且每条都放过歌
    // (有报告 —— `reclaim_orphans` 一条都不会碰)。
    for device in ["phone", "pc1"] {
        for _ in 0..5 {
            let (id,): (i64,) = sqlx::query_as(
                "INSERT INTO play_queues
                     (account_id, device_id, revision, next_entry_id)
                 VALUES ($1, $2, 1, 1) RETURNING id",
            )
            .bind(account.id)
            .bind(device)
            .fetch_one(&mut *tx)
            .await
            .expect("造残留应该成功");
            sqlx::query(
                "INSERT INTO play_queue_reports
                     (queue_id, device_id, epoch, state_seq,
                      applied_revision, round, position_ms, play_state)
                 VALUES ($1, $2, 1, 1, 1, 0, 0, 'playing')",
            )
            .bind(id)
            .bind(device)
            .execute(&mut *tx)
            .await
            .expect("造报告应该成功");
        }
    }
    assert_eq!(
        count_queues(&mut tx, account.id).await,
        queue::MAX_QUEUES_PER_ACCOUNT,
        "现场该正好卡在上限上"
    );

    // 平板第一次点歌。
    queue::create(
        &mut tx,
        account.id,
        "tablet",
        &[entry("a")],
    )
    .await
    .expect("新设备该能建队列,不必等别的设备先来一趟");

    assert_eq!(
        count_queues(&mut tx, account.id).await,
        3,
        "两台老设备各收成一条,加上平板自己那条"
    );
}
