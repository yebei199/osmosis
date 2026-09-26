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
use server::syncplay::group::{self, Intent};
use server::syncplay::roster::Roster;
use server::syncplay::signaling::{SharedRoster, Sink};
use sqlx::PgPool;
use tokio::sync::mpsc;

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
        "group-{tag}-{}-{}",
        std::process::id(),
        group::wall_now_us()
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
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: format!("歌 {id}"),
        alias: None,
        artists: vec!["LiSA".to_owned()],
        cover: None,
        duration_ms: 200_000,
    }
}

fn tracks() -> Vec<TrackDto> {
    ["a", "b", "c"].into_iter().map(track).collect()
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
