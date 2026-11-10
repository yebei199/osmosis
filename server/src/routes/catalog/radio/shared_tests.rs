//! 共享电台歌单(#186):各台读到同一份、加载新歌追加并通知各台、
//! 听过的挪进「已听过」、新歌立刻排进预取。

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use contract::{
    DeviceDto, ListenedDto, PlayedDto, RadioListDto,
    ServerSignal,
};
use similar_asserts::assert_eq;
use tokio::sync::mpsc;

use crate::AppState;
use crate::routes::play::archive::Archive;
use crate::routes::testing::{
    self, FakeUpstream, MemoryObjects, track_id,
    upstream_track,
};

use super::{
    MoreQuery, REFILL, TOP_UP_BELOW, list, more, top_up,
};

fn ids(tracks: &[contract::TrackDto]) -> Vec<String> {
    tracks.iter().map(|track| track.id.clone()).collect()
}

fn unfiltered() -> Query<MoreQuery> {
    Query(MoreQuery { filter: None })
}

/// 账号下挂一台在线设备,交出它的收件端。
fn online(
    state: &AppState,
    account_id: i64,
    device: &str,
) -> mpsc::Receiver<ServerSignal> {
    let (sink, inbox) = mpsc::channel(8);
    state.roster.lock().unwrap().join(
        account_id,
        DeviceDto {
            id: device.to_owned(),
            name: device.to_owned(),
        },
        sink,
    );
    inbox
}

/// 收件箱里有没有一条「电台歌单变了」。
fn told(inbox: &mut mpsc::Receiver<ServerSignal>) -> bool {
    std::iter::from_fn(|| inbox.try_recv().ok()).any(
        |message| message == ServerSignal::RadioChanged,
    )
}

/// 起一份带内存对象存储的 state(预取只在配了存储时入队),上游按顺序摆出 `batches`。
async fn fixture(
    case: &str,
    batches: Vec<Vec<String>>,
) -> (AppState, server::store::account::Account) {
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let fake = FakeUpstream::default();
    fake.fm_batches.lock().unwrap().extend(
        batches.into_iter().map(|batch| {
            batch
                .iter()
                .map(|id| upstream_track(id, "新"))
                .collect()
        }),
    );
    let state = AppState {
        archive: Some(Archive::new(Arc::new(
            MemoryObjects::default(),
        ))),
        ..testing::state(pool, testing::serve(fake).await)
    };
    (state, account)
}

/// AC-1:一台加载新歌,另一台收到通知,两台读到的歌单一样;再加载一次是追加。
#[tokio::test]
async fn loading_more_appends_and_every_device_sees_the_same_list()
 {
    let case = "radio_shared_more";
    let id = |n| track_id(case, n);
    let (state, account) = fixture(
        case,
        vec![
            (1..=REFILL).map(id).collect(),
            vec![id(REFILL + 1), id(REFILL + 2)],
        ],
    )
    .await;
    let mut phone = online(&state, account.id, "phone");
    let mut desk = online(&state, account.id, "desk");

    let empty =
        list(State(state.clone()), account.clone()).await;
    assert_eq!(
        empty.expect("读歌单").0,
        RadioListDto::default()
    );

    let first = more(
        State(state.clone()),
        account.clone(),
        unfiltered(),
    )
    .await
    .expect("加载新歌")
    .0;
    assert_eq!(
        ids(&first.tracks),
        (1..=REFILL).map(id).collect::<Vec<_>>(),
        "攒够一批就停,不把平台问空"
    );
    assert!(told(&mut phone) && told(&mut desk));

    let second = more(
        State(state.clone()),
        account.clone(),
        unfiltered(),
    )
    .await
    .expect("再加载")
    .0;
    assert_eq!(
        ids(&second.tracks),
        (1..=REFILL + 2).map(id).collect::<Vec<_>>()
    );
    let seen = list(State(state), account).await.unwrap().0;
    assert_eq!(seen, second);
}

/// 已经在歌单里、还没听的,平台再推一遍也不重复加。一首都没加上不广播。
#[tokio::test]
async fn a_track_already_listed_is_not_added_twice() {
    let case = "radio_shared_dupe";
    let id = |n| track_id(case, n);
    let (state, account) = fixture(
        case,
        vec![vec![id(1)], vec![id(1)], vec![id(1)]],
    )
    .await;
    let _ = more(
        State(state.clone()),
        account.clone(),
        unfiltered(),
    )
    .await
    .unwrap();
    let mut inbox = online(&state, account.id, "phone");

    let again = more(
        State(state.clone()),
        account.clone(),
        unfiltered(),
    )
    .await
    .unwrap()
    .0;

    assert_eq!(ids(&again.tracks), vec![id(1)]);
    assert!(!told(&mut inbox));
}

/// 起播 `track_id`,再补记听了 `listened_ms`(这首长 200 秒)。
async fn listen(
    state: &AppState,
    account: &server::store::account::Account,
    track_id: String,
    listened_ms: i64,
) {
    let ack = crate::routes::library::history::record_play(
        State(state.clone()),
        account.clone(),
        Json(PlayedDto {
            platform: "netease".to_owned(),
            track_id,
        }),
    )
    .await
    .expect("报播放")
    .0;
    crate::routes::library::history::report_listened(
        State(state.clone()),
        account.clone(),
        Path(ack.id),
        Json(ListenedDto {
            listened_ms,
            duration_ms: 200_000,
        }),
    )
    .await
    .expect("补记听了多久");
}

/// AC-2:听够了一首(过半),它从歌单挪进「已听过」,各台收到通知。
#[tokio::test]
async fn a_track_heard_through_moves_to_heard() {
    let case = "radio_shared_heard";
    let id = |n| track_id(case, n);
    let (state, account) =
        fixture(case, vec![vec![id(1), id(2), id(3)]])
            .await;
    let _ = more(
        State(state.clone()),
        account.clone(),
        unfiltered(),
    )
    .await
    .unwrap();
    let mut inbox = online(&state, account.id, "desk");

    listen(&state, &account, id(2), 100_000).await;

    assert!(told(&mut inbox));
    let now = list(State(state), account).await.unwrap().0;
    assert_eq!(ids(&now.tracks), vec![id(1), id(3)]);
    assert_eq!(ids(&now.heard), vec![id(2)]);
}

/// 出声那一刻不算听过,听了一点就切走也不算:歌留在主列表,各台不被打扰。
#[tokio::test]
async fn starting_or_skipping_early_is_not_hearing() {
    let case = "radio_shared_skipped";
    let id = |n| track_id(case, n);
    let (state, account) =
        fixture(case, vec![vec![id(1), id(2)]]).await;
    let _ = more(
        State(state.clone()),
        account.clone(),
        unfiltered(),
    )
    .await
    .unwrap();
    let mut inbox = online(&state, account.id, "desk");

    let _ = crate::routes::library::history::record_play(
        State(state.clone()),
        account.clone(),
        Json(PlayedDto {
            platform: "netease".to_owned(),
            track_id: id(1),
        }),
    )
    .await
    .expect("报播放");
    listen(&state, &account, id(2), 99_999).await;

    assert!(!told(&mut inbox));
    let now = list(State(state), account).await.unwrap().0;
    assert_eq!(ids(&now.tracks), vec![id(1), id(2)]);
    assert!(now.heard.is_empty());
}

/// 歌单抽干了、私人 FM 也一首新歌都给不出:以听过的歌为种子接心动,补满一批。
/// 线上就是这样见底的,未听 0 首、已听 401 首。
#[tokio::test]
async fn a_dry_fm_is_refilled_from_heart_mode() {
    let case = "radio_shared_heart_refill";
    let id = |n| track_id(case, n);
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let mut fake =
        FakeUpstream::logged_in_with("nuid", Vec::new());
    fake.playlists =
        vec![testing::liked_playlist("liked-7")];
    fake.fm_batches
        .lock()
        .unwrap()
        .push_back(vec![upstream_track(&id(1), "新")]);
    fake.listed = (2..=REFILL + 10)
        .map(|n| upstream_track(&id(n), "心动"))
        .collect();
    let state = AppState {
        archive: Some(Archive::new(Arc::new(
            MemoryObjects::default(),
        ))),
        ..testing::state(
            pool,
            testing::serve(fake.clone()).await,
        )
    };

    let first = more(
        State(state.clone()),
        account.clone(),
        unfiltered(),
    )
    .await
    .expect("补歌")
    .0;
    assert_eq!(
        first.tracks.len(),
        REFILL,
        "没听过种子也靠日推补满"
    );
    assert!(
        fake.heart_asks().is_empty(),
        "没有听过的歌就没有种子"
    );

    listen(&state, &account, id(1), 200_000).await;
    let second = more(State(state), account, unfiltered())
        .await
        .expect("再补")
        .0;

    let asks = fake.heart_asks();
    assert_eq!(asks[0].seed_track_id, id(1));
    assert_eq!(asks[0].playlist_id, "liked-7");
    assert_eq!(
        second.tracks.len(),
        REFILL + 9,
        "心动给的新歌都进了歌单,听过的那首不算"
    );
}

/// 不在电台歌单里的歌听完了,不打扰各台。
#[tokio::test]
async fn playing_something_else_does_not_notify() {
    let case = "radio_shared_quiet";
    let (state, account) = fixture(case, vec![]).await;
    let mut inbox = online(&state, account.id, "desk");

    listen(&state, &account, track_id(case, 1), 200_000)
        .await;

    assert!(!told(&mut inbox));
}

/// AC-3:加进来的新歌立刻排进预取队列(之后由 worker 存进 RustFS,
/// 那一段见 `routes::play::archive` 的 `a_queued_job_is_stored_and_leaves_the_queue`)。
#[tokio::test]
async fn new_tracks_are_queued_for_prefetch() {
    let case = "radio_shared_prefetch";
    let id = |n| track_id(case, n);
    let (state, account) =
        fixture(case, vec![vec![id(1), id(2), id(3)]])
            .await;

    let _ =
        more(State(state.clone()), account, unfiltered())
            .await
            .unwrap();

    let queued: Vec<String> = sqlx::query_scalar(
        "SELECT track_id FROM prefetch_jobs
         WHERE track_id = ANY($1) ORDER BY track_id",
    )
    .bind(vec![id(1), id(2), id(3)])
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(queued, vec![id(1), id(2), id(3)]);
}

/// 未听的不够:服务端自己补,不等客户端来要。
#[tokio::test]
async fn a_short_list_is_topped_up_by_the_server() {
    let case = "radio_shared_top_up";
    let id = |n| track_id(case, n);
    let (state, account) =
        fixture(case, vec![vec![id(1), id(2)]]).await;

    top_up(&state, &account, TOP_UP_BELOW)
        .await
        .expect("补歌");

    let now = list(State(state), account).await.unwrap().0;
    assert_eq!(ids(&now.tracks), vec![id(1), id(2)]);
}

/// 未听的够多就不问平台:补歌也是一趟上游请求。
#[tokio::test]
async fn a_full_list_is_left_alone() {
    let case = "radio_shared_full";
    let id = |n| track_id(case, n);
    let (state, account) = fixture(
        case,
        vec![
            (1..=REFILL).map(id).collect(),
            (REFILL + 1..=REFILL * 2).map(id).collect(),
            vec![id(REFILL * 2 + 1)],
        ],
    )
    .await;
    for _ in 0..2 {
        let _ = more(
            State(state.clone()),
            account.clone(),
            unfiltered(),
        )
        .await
        .unwrap();
    }

    top_up(&state, &account, REFILL * 2)
        .await
        .expect("看一眼");

    let now = list(State(state), account).await.unwrap().0;
    assert_eq!(now.tracks.len(), REFILL * 2);
}
