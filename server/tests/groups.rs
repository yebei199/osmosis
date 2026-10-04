//! 组的全局播放状态对着真库(#142)。
//!
//! 意图要落库、要跨服务端重启,这些是数据库的行为,所以打真库。`group::apply` 自己开事务、
//! 提交,测试没法回滚,于是每条测试用一个本进程独有的账号名,测完删掉账号(级联删掉组
//! 与队列)。

use std::sync::{Arc, Mutex};

use contract::{
    DeviceDto, GroupPickDto, GroupSeedDto, GroupStateDto,
    ServerSignal, TrackDto, TransportOpDto,
};
use server::store::account::register;
use server::store::db;
use server::store::queue::{self, EntryInput};
use server::syncplay::clock;
use server::syncplay::group::{self, Intent, timeline};
use server::syncplay::roster::Roster;
use server::syncplay::signaling::{SharedRoster, Sink};
use similar_asserts::assert_eq;
use sqlx::PgPool;
use tokio::sync::mpsc;

/// 恢复时必须清空的原始播放引用与时间线,按数据库列顺序取。
type StoredPlayback = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    bool,
    i64,
    i64,
    Option<i64>,
    bool,
    String,
    Vec<i64>,
);

const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

const INVITE: &str = "let-me-in";

async fn connect() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(
        |_| DEFAULT_DATABASE_URL.to_owned(),
    );
    db::connect(&url).await.unwrap_or_else(|err| {
        panic!("连不上数据库({url}): {err}\n起一个:just pg")
    })
}

/// 一个本进程独有的账号。
async fn account(pool: &PgPool, tag: &str) -> i64 {
    let name = format!(
        "group-{tag}-{}-{}-{}",
        std::process::id(),
        group::wall_now_us(),
        std::env::var("RADIO_OUTPUT_TEST_RUN")
            .unwrap_or_default()
    );
    let mut conn =
        pool.acquire().await.expect("取不到连接");
    register(
        &mut conn,
        &name,
        "correct horse",
        INVITE,
        INVITE,
    )
    .await
    .expect("注册应该成功")
    .id
}

async fn drop_account(pool: &PgPool, id: i64) {
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .expect("删不掉测试账号");
}

/// 名册里挂上这几台在线设备,返回名册与各自的收件箱。
fn online(
    account: i64,
    ids: &[&str],
) -> (SharedRoster, Vec<mpsc::Receiver<ServerSignal>>) {
    let mut roster = Roster::<Sink>::default();
    let mut inboxes = Vec::new();
    for id in ids {
        let (sink, inbox) = mpsc::channel(64);
        roster.join(
            account,
            DeviceDto {
                id: (*id).to_owned(),
                name: (*id).to_owned(),
            },
            sink,
        );
        inboxes.push(inbox);
    }
    (Arc::new(Mutex::new(roster)), inboxes)
}

/// 这台设备出册(与连接断开时 `signaling::serve` 做的一样)。
fn go_offline(
    roster: &SharedRoster,
    account: i64,
    id: &str,
) {
    let mut roster = roster.lock().unwrap();
    let (generation, _) = roster.join(
        account,
        DeviceDto {
            id: id.to_owned(),
            name: id.to_owned(),
        },
        mpsc::channel(1).0,
    );
    assert!(roster.leave(account, id, generation));
}

fn track(id: &str) -> TrackDto {
    TrackDto {
        artist_identities: Vec::new(),
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: format!("歌 {id}"),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: 200_000,
        album: None,
        facets: Default::default(),
    }
}

fn tracks() -> Vec<TrackDto> {
    ["a", "b", "c"].into_iter().map(track).collect()
}

/// 同一条电台队列持续发布,暂停和播放中的组都保留精确旧版;解除引用后恢复回收。
#[tokio::test]
async fn radio_output_retains_the_group_revision_until_released()
 {
    let pool = connect().await;
    for paused in [false, true] {
        let account = account(
            &pool,
            if paused {
                "retain-paused"
            } else {
                "retain-playing"
            },
        )
        .await;
        let (roster, _inboxes) =
            online(account, &["phone", "pc"]);
        let initial =
            phone_and_pc(&pool, &roster, account).await;
        let now = initial.now.expect("该有播放引用");
        if paused {
            group::apply(
                &pool,
                &roster,
                account,
                "phone",
                Intent::Transport(TransportOpDto::Pause),
            )
            .await
            .expect("暂停该成");
        }
        let mut revision = now.revision;
        for _ in 0..5 {
            let mut tx =
                pool.begin().await.expect("开事务");
            revision = queue::publish(
                &mut tx,
                account,
                now.queue_id,
                revision,
                &[EntryInput {
                    platform: "netease".to_owned(),
                    track_id: "new-radio".to_owned(),
                    title: "新电台曲目".to_owned(),
                    alias: None,
                    artists: Vec::new(),
                    cover: None,
                    duration_ms: 200_000,
                }],
            )
            .await
            .expect("发布新版")
            .revision;
            tx.commit().await.expect("提交新版");
        }
        let mut conn =
            pool.acquire().await.expect("取连接");
        let held = queue::whole(
            &mut conn,
            account,
            now.queue_id,
            now.revision,
        )
        .await;
        assert!(
            held.is_ok(),
            "组引用的旧版应保留: {held:?}"
        );
        drop(conn);
        let current = group::current(&pool, account)
            .await
            .expect("GET 组应成功")
            .expect("组在");
        assert_eq!(
            current
                .now
                .as_ref()
                .expect("播放引用在")
                .entry_id,
            now.entry_id
        );
        group::greet(&pool, &roster, account, "pc")
            .await
            .expect("重新入册应成功");
        let switched = group::apply(
            &pool,
            &roster,
            account,
            "phone",
            Intent::Outputs {
                outputs: vec!["pc".to_owned()],
                seed: None,
            },
        )
        .await
        .expect("切输出应成功")
        .expect("组在");
        assert_eq!(
            switched.now.expect("原播放还在").playing,
            !paused
        );
        let replacement = group::apply(
            &pool,
            &roster,
            account,
            "phone",
            Intent::Play(GroupPickDto::Tracks {
                tracks: vec![track("replacement-radio")],
                index: 0,
            }),
        )
        .await
        .expect("换队列该成")
        .expect("组仍在");
        assert_ne!(
            replacement.now.expect("新播放引用在").queue_id,
            now.queue_id,
            "先确认组已经解除旧队列引用"
        );
        let mut tx = pool.begin().await.expect("开事务");
        queue::publish(
            &mut tx,
            account,
            now.queue_id,
            revision,
            &[EntryInput {
                platform: "netease".to_owned(),
                track_id: "next-radio".to_owned(),
                title: "下一批".to_owned(),
                alias: None,
                artists: Vec::new(),
                cover: None,
                duration_ms: 200_000,
            }],
        )
        .await
        .expect("再发布");
        assert!(
            matches!(
                queue::whole(
                    &mut tx,
                    account,
                    now.queue_id,
                    now.revision
                )
                .await,
                Err(server::error::AppError::NotFound)
            ),
            "解除组引用的旧版该回收"
        );
        tx.commit().await.expect("提交");
        drop_account(&pool, account).await;
    }
}

/// 模拟生产失效版或缺失当前条目,仅删除本测试账号组指着的条目。
async fn damage_radio_group(
    pool: &PgPool,
    account: i64,
    missing_entry: bool,
) {
    sqlx::query(if missing_entry {
        "DELETE FROM play_queue_entries WHERE (queue_id, revision, entry_id) IN
         (SELECT queue_id, revision, entry_id FROM play_groups WHERE account_id = $1)"
    } else {
        "DELETE FROM play_queue_entries WHERE (queue_id, revision) IN
         (SELECT queue_id, revision FROM play_groups WHERE account_id = $1)"
    }).bind(account).execute(pool).await.expect("构造失效引用");
}

/// create 归并同设备旧队列时,保护组引用的精确旧队列,其余旧队列照常删除。
#[tokio::test]
async fn radio_output_collapse_retains_referenced_queues_only()
 {
    let pool = connect().await;
    let account = account(&pool, "collapse-group").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let before =
        phone_and_pc(&pool, &roster, account).await;
    let now = before.now.expect("原引用在");
    let obsolete: i64 = sqlx::query_scalar("INSERT INTO play_queues (account_id, device_id, revision, next_entry_id, updated_at)
        VALUES ($1, 'pc', 1, 1, now() - INTERVAL '1 day') RETURNING id")
        .bind(account).fetch_one(&pool).await.expect("本次无引用旧队列");
    sqlx::query("INSERT INTO play_queues (account_id, device_id, revision, next_entry_id, updated_at)
        VALUES ($1, 'pc', 1, 1, now() + INTERVAL '1 day')")
        .bind(account).execute(&pool).await.expect("本次同设备最新队列");
    let mut tx = pool.begin().await.expect("开事务");
    queue::create(
        &mut tx,
        account,
        "pc",
        &[EntryInput {
            platform: "netease".to_owned(),
            track_id: "new-radio".to_owned(),
            title: "新电台".to_owned(),
            alias: None,
            artists: Vec::new(),
            cover: None,
            duration_ms: 200_000,
        }],
    )
    .await
    .expect("真实 create 触发归并");
    tx.commit().await.expect("提交归并");
    let mut conn = pool.acquire().await.expect("取连接");
    let retained = queue::whole(
        &mut conn,
        account,
        now.queue_id,
        now.revision,
    )
    .await;
    assert!(
        retained.is_ok(),
        "归并应保护组旧队列: {retained:?}"
    );
    drop(conn);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM play_queues WHERE id = $1",
    )
    .bind(obsolete)
    .fetch_one(&pool)
    .await
    .expect("查未引用队列");
    assert_eq!(count, 0, "正常无引用旧队列仍回收");
    let after = group::current(&pool, account)
        .await
        .expect("组还可读")
        .expect("组在");
    assert_eq!(
        after.now.expect("保留播放").entry_id,
        now.entry_id
    );
    drop_account(&pool, account).await;
}

/// 用真实发布事务的队列锁编排种子采用,已提交组绝不能引用被回收版,两端有界完成。
#[tokio::test]
async fn radio_output_concurrent_seed_and_publish_keep_committed_references_readable()
 {
    let pool = connect().await;
    let account = account(&pool, "seed-publish-race").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let mut publisher =
        pool.begin().await.expect("开发布事务");
    let first = queue::create(
        &mut publisher,
        account,
        "phone",
        &[EntryInput {
            platform: "netease".to_owned(),
            track_id: "a".to_owned(),
            title: "a".to_owned(),
            alias: None,
            artists: Vec::new(),
            cover: None,
            duration_ms: 200_000,
        }],
    )
    .await
    .expect("建种子队列");
    publisher.commit().await.expect("提交种子");
    let actor = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await
        .expect("本次独立种子连接池");
    let actor_pid: i32 =
        sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&actor)
            .await
            .expect("种子连接身份");
    sqlx::query("SET statement_timeout = '10s'")
        .execute(&actor)
        .await
        .expect("种子数据库等待有界");
    let mut publisher =
        pool.begin().await.expect("真实发布事务");
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut *publisher)
        .await
        .expect("发布数据库等待有界");
    sqlx::query("SELECT id FROM play_queues WHERE id = $1 FOR UPDATE")
        .bind(first.queue_id).execute(&mut *publisher).await.expect("冻结发布临界区");
    let seed_pool = actor.clone();
    let seed_roster = roster.clone();
    let seed = GroupSeedDto {
        queue_id: first.queue_id,
        revision: first.revision,
        entry_id: first.entry_ids[0],
        position_ms: 30_000,
        playing: true,
    };
    let adopting = tokio::spawn(async move {
        group::apply(
            &seed_pool,
            &seed_roster,
            account,
            "phone",
            Intent::Outputs {
                outputs: vec!["pc".to_owned()],
                seed: Some(seed),
            },
        )
        .await
    });
    // 等数据库确认种子事务正在等锁;不靠 sleep 猜测是否到达临界区。
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                .bind(actor_pid).fetch_one(&pool).await.expect("查询本次事务等待");
            if blocked { break; }
            assert!(!adopting.is_finished(), "种子应与发布临界区交错");
            tokio::task::yield_now().await;
        }
    }).await.expect("种子应有界抵达数据库同步点");
    let mut revision = first.revision;
    for _ in 0..5 {
        revision = queue::publish(
            &mut publisher,
            account,
            first.queue_id,
            revision,
            &[EntryInput {
                platform: "netease".to_owned(),
                track_id: "b".to_owned(),
                title: "b".to_owned(),
                alias: None,
                artists: Vec::new(),
                cover: None,
                duration_ms: 200_000,
            }],
        )
        .await
        .expect("真实发布与回收")
        .revision;
    }
    publisher.commit().await.expect("发布应无死锁提交");
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        adopting,
    )
    .await
    .expect("种子请求应有界完成")
    .expect("种子任务未 panic");
    match outcome {
        Ok(Some(state)) => {
            let now = state
                .now
                .expect("采用成功必须有有效播放引用");
            let mut conn =
                pool.acquire().await.expect("验证连接");
            let entries = queue::whole(
                &mut conn,
                account,
                now.queue_id,
                now.revision,
            )
            .await
            .expect("已提交组引用必须可读");
            assert!(entries.iter().any(|entry| {
                entry.entry_id == now.entry_id
            }));
        }
        Err(server::error::AppError::NotFound) => {
            assert!(
                group::current(&pool, account)
                    .await
                    .expect("拒绝后仍可读")
                    .is_none(),
                "被拒种子不提交半个组"
            );
        }
        other => panic!(
            "竞态只许成功采用有效版或明确拒绝过期种子: {other:?}"
        ),
    }
    actor.close().await;
    drop_account(&pool, account).await;
}

/// GET 和入册都持久恢复,清空时间线、保留关系,重复读不再递增版本。
#[tokio::test]
async fn radio_output_read_and_greet_repair_invalid_references()
 {
    let pool = connect().await;
    for damage in ["revision", "entry", "queue"] {
        let account =
            account(&pool, &format!("missing-{damage}"))
                .await;
        let (roster, mut inboxes) =
            online(account, &["phone", "pc"]);
        let before =
            phone_and_pc(&pool, &roster, account).await;
        if damage == "queue" {
            sqlx::query("DELETE FROM play_queues WHERE id IN (SELECT queue_id FROM play_groups WHERE account_id = $1)")
                .bind(account).execute(&pool).await.expect("构造缺队列");
        } else {
            damage_radio_group(
                &pool,
                account,
                damage == "entry",
            )
            .await;
        }
        for inbox in &mut inboxes {
            while inbox.try_recv().is_ok() {}
        }
        group::greet(&pool, &roster, account, "pc")
            .await
            .expect("入册应恢复");
        let restored = group::current(&pool, account)
            .await
            .expect("读取应恢复")
            .expect("成员还在");
        assert_eq!(restored.members, before.members);
        assert_eq!(restored.outputs, before.outputs);
        assert!(restored.now.is_none(), "不能猜旧歌");
        assert_eq!(restored.version, before.version + 1);
        assert!(
            matches!(inboxes[1].try_recv(), Ok(ServerSignal::GroupState { state: Some(state) })
            if state.version == restored.version && state.now.is_none())
        );
        let stored: StoredPlayback =
            sqlx::query_as("SELECT queue_id, revision, entry_id, playing, position_us, anchor_wall_us,
                boundary_wall_us, shuffled, loop_mode, play_order FROM play_groups WHERE account_id = $1")
            .bind(account).fetch_one(&pool).await.expect("读持久状态");
        assert_eq!(
            stored,
            (
                None,
                None,
                None,
                false,
                0,
                0,
                None,
                false,
                "off".to_owned(),
                Vec::new()
            )
        );
        assert_eq!(
            group::current(&pool, account)
                .await
                .expect("再读")
                .expect("组在")
                .version,
            restored.version
        );
        drop_account(&pool, account).await;
    }
}

/// 健康空组读取和入册不触发修复或增加版本。
#[tokio::test]
async fn radio_output_healthy_empty_group_is_not_repaired()
{
    let pool = connect().await;
    let account = account(&pool, "healthy-empty").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let before = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Outputs {
            outputs: vec!["pc".to_owned()],
            seed: None,
        },
    )
    .await
    .expect("建健康空组")
    .expect("成员在");
    let stored: String = sqlx::query_scalar("SELECT to_jsonb(g)::text FROM play_groups g WHERE account_id = $1")
        .bind(account).fetch_one(&pool).await.expect("读健康空组");
    group::greet(&pool, &roster, account, "pc")
        .await
        .expect("健康空组入册");
    let after = group::current(&pool, account)
        .await
        .expect("读健康空组")
        .expect("组在");
    assert_eq!(after, before);
    let unchanged: String = sqlx::query_scalar("SELECT to_jsonb(g)::text FROM play_groups g WHERE account_id = $1")
        .bind(account).fetch_one(&pool).await.expect("再读健康空组");
    assert_eq!(unchanged, stored);
    drop_account(&pool, account).await;
}

/// 同一次切输出恢复,或客户端已先收到空组,都接续有效本机种子;无种子保持空组。
#[tokio::test]
async fn radio_output_reseeds_repaired_groups_in_both_entry_paths()
 {
    let pool = connect().await;
    for read_first in [false, true] {
        for with_seed in [false, true] {
            let account = account(
                &pool,
                &format!("reseed-{read_first}-{with_seed}"),
            )
            .await;
            let (roster, _inboxes) =
                online(account, &["phone", "pc"]);
            let before =
                phone_and_pc(&pool, &roster, account).await;
            damage_radio_group(&pool, account, false).await;
            if read_first {
                group::current(&pool, account)
                    .await
                    .expect("先读恢复");
            }
            let mut tx =
                pool.begin().await.expect("开事务");
            let local = queue::create(
                &mut tx,
                account,
                "phone",
                &[EntryInput {
                    platform: "netease".to_owned(),
                    track_id: "current-radio".to_owned(),
                    title: "当前电台".to_owned(),
                    alias: None,
                    artists: Vec::new(),
                    cover: None,
                    duration_ms: 200_000,
                }],
            )
            .await
            .expect("本机有效队列");
            tx.commit().await.expect("提交种子");
            let after = group::apply(
                &pool,
                &roster,
                account,
                "phone",
                Intent::Outputs {
                    outputs: vec!["pc".to_owned()],
                    seed: with_seed.then_some(
                        GroupSeedDto {
                            queue_id: local.queue_id,
                            revision: local.revision,
                            entry_id: local.entry_ids[0],
                            position_ms: 30_000,
                            playing: true,
                        },
                    ),
                },
            )
            .await
            .expect("损坏组切输出应成功")
            .expect("组在");
            assert_eq!(after.members, before.members);
            assert_eq!(after.outputs, vec!["pc"]);
            assert!(after.version > before.version);
            if with_seed {
                let now = after.now.expect("接续电台");
                assert_eq!(
                    (
                        now.queue_id,
                        now.revision,
                        now.entry_id
                    ),
                    (
                        local.queue_id,
                        local.revision,
                        local.entry_ids[0]
                    )
                );
                assert_eq!(now.track.id, "current-radio");
                assert!(
                    now.playing
                        && now.position_us >= 30_000_000
                );
            } else {
                assert!(
                    after.now.is_none(),
                    "无本机播放不猜歌"
                );
            }
            drop_account(&pool, account).await;
        }
    }
}

/// 损坏组仍能离组和点新歌,组外设备控制仍拒绝。
#[tokio::test]
async fn radio_output_damage_does_not_block_leave_or_new_play()
 {
    let pool = connect().await;
    for leave in [false, true] {
        let account = account(
            &pool,
            if leave {
                "damaged-leave"
            } else {
                "damaged-play"
            },
        )
        .await;
        let (roster, _inboxes) =
            online(account, &["phone", "pc", "stranger"]);
        phone_and_pc(&pool, &roster, account).await;
        damage_radio_group(&pool, account, false).await;
        let denied = group::apply(
            &pool,
            &roster,
            account,
            "stranger",
            Intent::Play(GroupPickDto::Tracks {
                tracks: tracks(),
                index: 0,
            }),
        )
        .await;
        assert!(
            matches!(
                denied,
                Err(server::error::AppError::Invalid(_))
            ),
            "仍检查成员权限: {denied:?}"
        );
        let intent = if leave {
            Intent::Leave
        } else {
            Intent::Play(GroupPickDto::Tracks {
                tracks: tracks(),
                index: 0,
            })
        };
        let after = group::apply(
            &pool, &roster, account, "phone", intent,
        )
        .await
        .expect("损坏引用不挡操作")
        .expect("组在");
        if leave {
            assert_eq!(after.members, vec!["pc"]);
            assert!(after.now.is_none());
        } else {
            assert_eq!(
                after.now.expect("新歌在放").track.id,
                "a"
            );
        }
        drop_account(&pool, account).await;
    }
}

/// 空组的种子必须同账号、同版本、同条目;正常组收到种子仍沿用原播放。
#[tokio::test]
async fn radio_output_seed_validation_and_valid_playback_are_preserved()
 {
    let pool = connect().await;
    let foreign = account(&pool, "seed-foreign").await;
    let account = account(&pool, "seed-validation").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let valid = phone_and_pc(&pool, &roster, account).await;
    let now = valid.now.clone().expect("有效播放");
    let held = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Outputs {
            outputs: vec!["pc".to_owned()],
            seed: Some(GroupSeedDto {
                queue_id: now.queue_id,
                revision: now.revision,
                entry_id: -1,
                position_ms: 0,
                playing: false,
            }),
        },
    )
    .await
    .expect("有效组忽略种子")
    .expect("组在");
    assert_eq!(
        held.now.expect("原歌在").entry_id,
        now.entry_id
    );
    let mut tx = pool.begin().await.expect("开事务");
    let outsider = queue::create(
        &mut tx,
        foreign,
        "pc",
        &[EntryInput {
            platform: "netease".to_owned(),
            track_id: "foreign".to_owned(),
            title: "别人的歌".to_owned(),
            alias: None,
            artists: Vec::new(),
            cover: None,
            duration_ms: 200_000,
        }],
    )
    .await
    .expect("外账号队列");
    tx.commit().await.expect("提交");
    group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Leave,
    )
    .await
    .expect("离组");
    group::apply(
        &pool,
        &roster,
        account,
        "pc",
        Intent::Leave,
    )
    .await
    .expect("散组");
    for (case, (queue_id, revision, entry_id)) in [
        (
            outsider.queue_id,
            outsider.revision,
            outsider.entry_ids[0],
        ),
        (now.queue_id, now.revision + 99, now.entry_id),
        (now.queue_id, now.revision, -1),
    ]
    .into_iter()
    .enumerate()
    {
        let before: String = sqlx::query_scalar("SELECT to_jsonb(g)::text FROM play_groups g WHERE account_id = $1")
            .bind(account).fetch_one(&pool).await.expect("读取失败请求前状态");
        let denied = group::apply(
            &pool,
            &roster,
            account,
            "phone",
            Intent::Outputs {
                outputs: vec!["pc".to_owned()],
                seed: Some(GroupSeedDto {
                    queue_id,
                    revision,
                    entry_id,
                    position_ms: 0,
                    playing: true,
                }),
            },
        )
        .await;
        if case < 2 {
            assert!(
                matches!(
                    denied,
                    Err(server::error::AppError::NotFound)
                ),
                "跨账号/缺版本须 NotFound: {denied:?}"
            );
        } else {
            assert!(
                matches!(
                    denied,
                    Err(server::error::AppError::Invalid(
                        "那一首不在组队列里"
                    ))
                ),
                "缺条目须领域拒绝: {denied:?}"
            );
        }
        let after: String = sqlx::query_scalar("SELECT to_jsonb(g)::text FROM play_groups g WHERE account_id = $1")
            .bind(account).fetch_one(&pool).await.expect("读取失败请求后状态");
        assert_eq!(
            after, before,
            "被拒请求所有持久状态都应回滚"
        );
    }
    drop_account(&pool, account).await;
    drop_account(&pool, foreign).await;
}

/// 数据库连接故障照常报错,不伪造一个恢复后的空组。
#[tokio::test]
async fn radio_output_database_errors_are_not_recovered() {
    let pool = connect().await;
    pool.close().await;
    assert!(matches!(
        group::current(&pool, 0).await,
        Err(server::error::AppError::Db(_))
    ));
}

/// 手机只当遥控器,pc 出声,从 pc 本机正在放的那一批的第 2 条接着放。
async fn phone_and_pc(
    pool: &PgPool,
    roster: &SharedRoster,
    account: i64,
) -> GroupStateDto {
    let inputs: Vec<EntryInput> = tracks()
        .iter()
        .map(|track| EntryInput {
            platform: track.platform.clone(),
            track_id: track.id.clone(),
            title: track.title.clone(),
            alias: None,
            artists: track.artists.clone(),
            cover: None,
            duration_ms: track.duration_ms,
        })
        .collect();
    let mut tx = pool.begin().await.expect("开事务失败");
    let seeded =
        queue::create(&mut tx, account, "pc", &inputs)
            .await
            .expect("建队列该成");
    tx.commit().await.expect("提交失败");

    group::apply(
        pool,
        roster,
        account,
        "phone",
        Intent::Outputs {
            outputs: vec!["pc".to_owned()],
            seed: Some(GroupSeedDto {
                queue_id: seeded.queue_id,
                revision: seeded.revision,
                entry_id: seeded.entry_ids[1],
                position_ms: 30_000,
                playing: true,
            }),
        },
    )
    .await
    .expect("建组该成")
    .expect("组该在")
}

/// 从本机正在放的那一份接着放;成员是发起的手机与出声的 pc。
#[tokio::test]
async fn a_group_carries_on_from_the_seed() {
    let pool = connect().await;
    let account = account(&pool, "seed").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);

    let state = phone_and_pc(&pool, &roster, account).await;

    assert_eq!(state.members, vec!["phone", "pc"]);
    assert_eq!(state.outputs, vec!["pc"]);
    let now = state.now.expect("该在放");
    assert_eq!(now.track.id, "b");
    assert!(now.playing);
    assert!(now.position_us >= 30_000_000);
    assert_eq!(
        now.next.map(|next| next.track.id),
        Some("c".to_owned())
    );
    drop_account(&pool, account).await;
}

/// 任何成员都能控制,并且每一下都广播给账号下每台在线设备(双向可控,AC-2)。
#[tokio::test]
async fn any_member_steers_and_everyone_hears_it() {
    let pool = connect().await;
    let account = account(&pool, "steer").await;
    let (roster, mut inboxes) =
        online(account, &["phone", "pc"]);
    phone_and_pc(&pool, &roster, account).await;
    for inbox in &mut inboxes {
        while inbox.try_recv().is_ok() {}
    }

    let paused = group::apply(
        &pool,
        &roster,
        account,
        "pc",
        Intent::Transport(TransportOpDto::Pause),
    )
    .await
    .expect("出声设备暂停该成")
    .expect("组该在");
    assert!(!paused.now.as_ref().expect("该有歌").playing);

    let picked = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Play(GroupPickDto::Tracks {
            tracks: tracks(),
            index: 2,
        }),
    )
    .await
    .expect("遥控器点歌该成")
    .expect("组该在");
    let now = picked.now.expect("该在放");
    assert_eq!(now.track.id, "c");
    assert!(now.playing);
    assert!(picked.version > paused.version);
    // 组里一次点歌只记一条起播,由服务端记(几台一起响还是那一次)。
    let (plays,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM play_events WHERE account_id = $1",
    )
    .bind(account)
    .fetch_one(&pool)
    .await
    .expect("数得出起播");
    assert_eq!(plays, 1, "种子那一份不算新起播,点歌算一次");

    for inbox in &mut inboxes {
        let mut last = None;
        while let Ok(message) = inbox.try_recv() {
            last = Some(message);
        }
        assert!(
            matches!(
                last,
                Some(ServerSignal::GroupState { state: Some(ref state) })
                    if state.version == picked.version
            ),
            "每台都该收到最新那一版,得到 {last:?}"
        );
    }
    drop_account(&pool, account).await;
}

/// 组外设备点歌被拒:组外点歌走本机。
#[tokio::test]
async fn a_stranger_cannot_steer_the_group() {
    let pool = connect().await;
    let account = account(&pool, "stranger").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc", "tablet"]);
    phone_and_pc(&pool, &roster, account).await;

    let refused = group::apply(
        &pool,
        &roster,
        account,
        "tablet",
        Intent::Transport(TransportOpDto::Next),
    )
    .await;

    assert!(refused.is_err());
    drop_account(&pool, account).await;
}

/// 服务端重启:状态从库里读回来,版本号不回退,组照样能控制(AC-4)。重启期间出声设备
/// 全断开了,照掉线规则 ② 暂停在服务端最后还活着的那一刻(#142 F-5)。
#[tokio::test]
async fn the_group_survives_a_server_restart() {
    let pool = connect().await;
    let account = account(&pool, "restart").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let before =
        phone_and_pc(&pool, &roster, account).await;
    let died = group::wall_now_us();

    // 新进程:名册是空的,库还在。启动时先把还在放的组补暂停。
    let restarted = connect().await;
    assert!(
        group::pause_stranded_one(
            &restarted, account, died
        )
        .await
        .expect("补暂停该成"),
        "重启前组在放,该补暂停"
    );
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let recovered = group::current(&restarted, account)
        .await
        .expect("读得出来")
        .expect("组该还在");
    assert_eq!(recovered.version, before.version + 1);
    assert_eq!(recovered.members, before.members);
    let now = recovered.now.as_ref().expect("该有歌");
    assert_eq!(now.track.id, "b");
    assert!(
        !now.playing,
        "服务端挂掉时没有出声设备在线,该暂停"
    );
    assert!(
        now.position_us >= 30_000_000,
        "位置停在断开那一刻,不回到种子之前: {}",
        now.position_us
    );

    let after = group::apply(
        &restarted,
        &roster,
        account,
        "phone",
        Intent::Transport(TransportOpDto::Next),
    )
    .await
    .expect("重启后照样能控制")
    .expect("组该在");
    assert!(
        after.version > recovered.version,
        "版本号不回退"
    );
    assert_eq!(
        after.now.map(|now| now.track.id),
        Some("c".to_owned())
    );
    drop_account(&pool, account).await;
}

/// 出声设备真正放完就推进;另一台迟到的同一份报告不再推进(AC-9)。
#[tokio::test]
async fn the_first_output_to_finish_advances_the_group() {
    let pool = connect().await;
    let account = account(&pool, "advance").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let before =
        phone_and_pc(&pool, &roster, account).await;
    let entry =
        before.now.as_ref().expect("该有歌").entry_id;

    let advanced = group::advance(
        &pool,
        &roster,
        account,
        "pc",
        entry,
        before.version as i64,
    )
    .await
    .expect("报放完该成")
    .expect("组该在");
    assert_eq!(advanced.version, before.version + 1);
    assert_eq!(
        advanced
            .now
            .as_ref()
            .map(|now| now.track.id.clone()),
        Some("c".to_owned())
    );

    let late = group::advance(
        &pool,
        &roster,
        account,
        "pc",
        entry,
        before.version as i64,
    )
    .await
    .expect("迟到的报告不是错")
    .expect("组该在");
    assert_eq!(
        late.version, advanced.version,
        "迟到的不加版本"
    );
    assert_eq!(
        late.now.map(|now| now.track.id),
        Some("c".to_owned())
    );
    drop_account(&pool, account).await;
}

/// 新起一首先锚远一点,等出声设备取流;在线的出声设备都报了就绪,起播提前到「现在 +
/// LEAD」并广播,不另记一次起播;再报一遍不加版本(#154)。
#[tokio::test]
async fn a_new_track_starts_once_every_output_is_ready() {
    let pool = connect().await;
    let account = account(&pool, "ready").await;
    let (roster, mut inboxes) =
        online(account, &["phone", "pc"]);
    phone_and_pc(&pool, &roster, account).await;

    let next = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Transport(TransportOpDto::Next),
    )
    .await
    .expect("下一首该成")
    .expect("组该在");
    let waiting = next.now.clone().expect("该在放");
    let issued = clock::now_us();
    assert!(
        waiting.anchor_us >= issued + 2_000_000,
        "没人报就绪时起播该在两秒开外,离现在只有 {}us",
        waiting.anchor_us.saturating_sub(issued)
    );
    for inbox in &mut inboxes {
        while inbox.try_recv().is_ok() {}
    }

    let ready = group::ready(
        &pool,
        &roster,
        account,
        "pc",
        waiting.entry_id,
        next.version as i64,
    )
    .await
    .expect("报就绪该成")
    .expect("组该在");
    let started = ready.now.clone().expect("该在放");
    assert_eq!(ready.version, next.version + 1);
    assert_eq!(started.entry_id, waiting.entry_id);
    assert!(
        started.anchor_us
            <= clock::now_us() + timeline::LEAD_US as u64,
        "都就绪了该在 LEAD 之内开走"
    );
    let (plays,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM play_events WHERE account_id = $1",
    )
    .bind(account)
    .fetch_one(&pool)
    .await
    .expect("数得出起播");
    assert_eq!(plays, 1, "提前开走不是又起了一首");
    for inbox in &mut inboxes {
        assert!(
            matches!(
                inbox.try_recv(),
                Ok(ServerSignal::GroupState { state: Some(ref state) })
                    if state.version == ready.version
            ),
            "提前开走要广播"
        );
    }

    let again = group::ready(
        &pool,
        &roster,
        account,
        "pc",
        started.entry_id,
        ready.version as i64,
    )
    .await
    .expect("再报一遍不是错")
    .expect("组该在");
    assert_eq!(
        again.version, ready.version,
        "再报不加版本"
    );
    drop_account(&pool, account).await;
}

/// 最后一台出声设备出册:立刻暂停;只当遥控器的出册不动(AC-3)。
#[tokio::test]
async fn the_last_output_leaving_pauses_the_group() {
    let pool = connect().await;
    let account = account(&pool, "offline").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    phone_and_pc(&pool, &roster, account).await;

    go_offline(&roster, account, "phone");
    group::device_left(&pool, &roster, account, "phone")
        .await
        .expect("遥控器出册");
    let still = group::current(&pool, account)
        .await
        .unwrap()
        .unwrap();
    assert!(
        still.now.as_ref().unwrap().playing,
        "遥控器掉线不影响出声"
    );

    go_offline(&roster, account, "pc");
    group::device_left(&pool, &roster, account, "pc")
        .await
        .expect("出声设备出册");
    let paused = group::current(&pool, account)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !paused.now.as_ref().unwrap().playing,
        "最后一台出声设备走了就暂停"
    );
    assert!(paused.version > still.version);
    drop_account(&pool, account).await;
}

/// 出声设备退出组:它离开成员与出声设备,组还在;最后一个成员也走了,组就散了。
#[tokio::test]
async fn leaving_takes_the_device_out_and_the_last_one_dissolves()
 {
    let pool = connect().await;
    let account = account(&pool, "leave").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    phone_and_pc(&pool, &roster, account).await;

    let left = group::apply(
        &pool,
        &roster,
        account,
        "pc",
        Intent::Leave,
    )
    .await
    .unwrap()
    .expect("手机还在,组还在");
    assert_eq!(left.members, vec!["phone"]);
    assert!(left.outputs.is_empty());

    let gone = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Leave,
    )
    .await
    .unwrap();
    assert_eq!(gone, None);
    drop_account(&pool, account).await;
}

/// 电台续歌(#165):组队列续上新的一版,已在队列里的不再进,在放的那一首不换、不重起,
/// 也不多记一条起播;拿着旧的一版再续被拒。
#[tokio::test]
async fn appending_extends_the_group_queue_in_place() {
    let pool = connect().await;
    let account = account(&pool, "append").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let before = phone_and_pc(&pool, &roster, account)
        .await
        .now
        .expect("该在放");

    let appended = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Append {
            queue: (before.queue_id, before.revision),
            tracks: vec![track("c"), track("d")],
        },
    )
    .await
    .expect("成员续歌该成")
    .expect("组该在");
    let after = appended.now.expect("该在放");
    assert_eq!(after.queue_id, before.queue_id);
    assert!(after.revision > before.revision);
    assert_eq!(after.entry_id, before.entry_id);
    assert_eq!(after.track.id, "b");
    assert!(after.playing);

    let mut conn =
        pool.acquire().await.expect("取不到连接");
    let ids: Vec<String> = queue::whole(
        &mut conn,
        account,
        after.queue_id,
        after.revision,
    )
    .await
    .expect("读得到新的一版")
    .into_iter()
    .map(|entry| entry.track_id)
    .collect();
    assert_eq!(ids, ["a", "b", "c", "d"]);
    let (plays,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM play_events WHERE account_id = $1",
    )
    .bind(account)
    .fetch_one(&pool)
    .await
    .expect("数得出起播");
    assert_eq!(plays, 0, "续歌不是新起播");

    let stale = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Append {
            queue: (before.queue_id, before.revision),
            tracks: vec![track("e")],
        },
    )
    .await;
    assert!(stale.is_err(), "旧的一版不该续得上");
    drop(conn);
    drop_account(&pool, account).await;
}

/// 重复电台续取被明确拒绝,组、队列全部版本与起播记录保持原样。
#[tokio::test]
async fn appending_only_existing_tracks_keeps_persistent_state_unchanged()
 {
    let pool = connect().await;
    let account = account(&pool, "append-duplicate").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);
    let now = phone_and_pc(&pool, &roster, account)
        .await
        .now
        .expect("组有播放引用");
    let before = append_snapshot(&pool, account).await;
    let outcome = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Append {
            queue: (now.queue_id, now.revision),
            tracks: vec![track("a"), track("b")],
        },
    )
    .await;
    assert!(matches!(
        outcome,
        Err(server::error::AppError::Invalid(
            "续来的歌都已经在组队列里"
        ))
    ));
    assert_eq!(
        append_snapshot(&pool, account).await,
        before
    );
    drop_account(&pool, account).await;
}

/// 按稳定主键排序保存这次账号的组、所有队列版本和起播记录。
async fn append_snapshot(
    pool: &PgPool,
    account: i64,
) -> String {
    sqlx::query_scalar(
        "SELECT jsonb_build_object(
            'group', (SELECT to_jsonb(g) FROM play_groups g WHERE account_id = $1),
            'queues', (SELECT jsonb_agg(to_jsonb(q) ORDER BY id) FROM play_queues q WHERE account_id = $1),
            'entries', (SELECT jsonb_agg(to_jsonb(e) ORDER BY e.queue_id, e.revision, e.entry_id)
                FROM play_queue_entries e JOIN play_queues q ON q.id = e.queue_id WHERE q.account_id = $1),
            'plays', (SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM play_events p WHERE account_id = $1)
        )::text"
    ).bind(account).fetch_one(pool).await.expect("保存续取前后持久状态")
}

/// 组的出声设备里挂着一台已经不在线的(#165):独奏的设备加进来时带上了它,服务端把它
/// 剔掉、照常加入;新点的设备不在线仍然拒。
#[tokio::test]
async fn joining_drops_offline_outputs_instead_of_refusing()
{
    let pool = connect().await;
    let account = account(&pool, "ghost").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc", "tab"]);
    phone_and_pc(&pool, &roster, account).await;
    go_offline(&roster, account, "pc");

    let joined = group::apply(
        &pool,
        &roster,
        account,
        "tab",
        Intent::Outputs {
            outputs: vec![
                "pc".to_owned(),
                "tab".to_owned(),
            ],
            seed: None,
        },
    )
    .await
    .expect("离线的旧出声设备不该挡住加入")
    .expect("组该在");
    assert_eq!(joined.outputs, vec!["tab"]);
    assert!(joined.members.contains(&"tab".to_owned()));

    let stranger = group::apply(
        &pool,
        &roster,
        account,
        "tab",
        Intent::Outputs {
            outputs: vec![
                "tab".to_owned(),
                "nobody".to_owned(),
            ],
            seed: None,
        },
    )
    .await;
    assert!(stranger.is_err(), "新点的设备不在线仍该拒");
    drop_account(&pool, account).await;
}

/// 组里换歌与独奏一致,同样跳过命中屏蔽规则的歌(#167):自然放完与「下一首」都跳过
/// 屏蔽队列里的 "b",落到再下一首,`play_events` 里没有被屏蔽那首。
#[tokio::test]
async fn the_group_skips_a_blocked_track_on_advance_and_next()
 {
    let pool = connect().await;
    let account = account(&pool, "block-skip").await;
    let (roster, _inboxes) =
        online(account, &["phone", "pc"]);

    let inputs: Vec<EntryInput> = tracks()
        .iter()
        .map(|track| EntryInput {
            platform: track.platform.clone(),
            track_id: track.id.clone(),
            title: track.title.clone(),
            alias: None,
            artists: track.artists.clone(),
            cover: None,
            duration_ms: track.duration_ms,
        })
        .collect();
    let mut tx = pool.begin().await.expect("开事务失败");
    let seeded =
        queue::create(&mut tx, account, "pc", &inputs)
            .await
            .expect("建队列该成");
    tx.commit().await.expect("提交失败");

    {
        let mut conn =
            pool.acquire().await.expect("取不到连接");
        server::store::blocks::create(
            &mut conn,
            account,
            contract::BlockKind::Track,
            "b",
            None,
        )
        .await
        .expect("建屏蔽规则该成");
    }

    let before = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Outputs {
            outputs: vec!["pc".to_owned()],
            seed: Some(GroupSeedDto {
                queue_id: seeded.queue_id,
                revision: seeded.revision,
                entry_id: seeded.entry_ids[0],
                position_ms: 0,
                playing: true,
            }),
        },
    )
    .await
    .expect("建组该成")
    .expect("组该在");
    let entry =
        before.now.as_ref().expect("该有歌").entry_id;

    let advanced = group::advance(
        &pool,
        &roster,
        account,
        "pc",
        entry,
        before.version as i64,
    )
    .await
    .expect("报放完该成")
    .expect("组该在");
    assert_eq!(
        advanced
            .now
            .as_ref()
            .map(|now| now.track.id.clone()),
        Some("c".to_owned()),
        "自然放完该跳过被屏蔽的 b,落到 c"
    );

    let stepped = group::apply(
        &pool,
        &roster,
        account,
        "phone",
        Intent::Transport(TransportOpDto::Prev),
    )
    .await
    .expect("上一首该成")
    .expect("组该在");
    assert_eq!(
        stepped.now.map(|now| now.track.id),
        Some("a".to_owned()),
        "上一首同样该跳过被屏蔽的 b,落到 a"
    );

    let hits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM play_events
         WHERE account_id = $1 AND track_id = 'b'",
    )
    .bind(account)
    .fetch_one(&pool)
    .await
    .expect("查 play_events 该成");
    assert_eq!(
        hits, 0,
        "被屏蔽的 b 不该起播、不该留下记录"
    );
    drop_account(&pool, account).await;
}
