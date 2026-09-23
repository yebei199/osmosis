//! 听过的歌存进对象存储:存、不重复存、试听不存、`/played` 真的会触发;
//! 再播时从对象存储交付,它出岔子时退回网易云。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Json;
use axum::extract::State;
use contract::PlayedDto;
use similar_asserts::assert_eq;

use server::bangdream::proto::PlaySource;
use server::store::account::Account;
use server::store::archive as ledger;
use server::store::playlist::TrackRef;

use crate::AppState;
use crate::routes::testing::{
    self, FakeUpstream, MemoryObjects,
};

use super::{Archive, keep, quality};

/// 在随机端口上摆一段字节,并数它被取了几次。
async fn serve_counted(
    body: Vec<u8>,
) -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");
    let app = axum::Router::new().route(
        "/audio",
        axum::routing::get(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            let body = body.clone();
            async move { body }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (format!("http://{addr}/audio"), hits)
}

/// 一首歌的字节,够认得出来就行。
fn audio() -> Vec<u8> {
    (0..4096u32).map(|i| i as u8).collect()
}

struct Fixture {
    state: AppState,
    account: Account,
    objects: Arc<MemoryObjects>,
    hits: Arc<AtomicUsize>,
}

/// 摆好假上游、内存对象存储与账号;上一轮留下的账目按 id 前缀清掉。
async fn fixture(case: &str, trial: bool) -> Fixture {
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    sqlx::query(
        "DELETE FROM stored_tracks WHERE track_id LIKE $1",
    )
    .bind(format!("{case}-%"))
    .execute(&pool)
    .await
    .expect("清账目失败");

    let (url, hits) = serve_counted(audio()).await;
    let fake = FakeUpstream {
        play_source: Some(PlaySource {
            url,
            format: "FLAC".to_owned(),
            bit_rate: 999_000,
            trial,
            ..PlaySource::default()
        }),
        ..FakeUpstream::logged_in_with("42", vec![])
    };
    let objects = Arc::new(MemoryObjects::default());
    let state = AppState {
        archive: Some(Archive::new(objects.clone())),
        ..testing::state(pool, testing::serve(fake).await)
    };

    Fixture {
        state,
        account,
        objects,
        hits,
    }
}

fn netease(track_id: &str) -> TrackRef {
    TrackRef {
        platform: "netease".to_owned(),
        track_id: track_id.to_owned(),
    }
}

async fn row(
    state: &AppState,
    track_id: &str,
) -> Option<ledger::StoredTrack> {
    let mut conn = state.pool.acquire().await.unwrap();
    ledger::find(&mut conn, "netease", track_id, &quality())
        .await
        .expect("查账目失败")
}

/// 存进去的是上游的原始字节与格式,键按「曲目/档位.格式」,账目记着字节数。
#[tokio::test]
async fn a_played_track_is_stored_as_is() {
    let f = fixture("ar_store", false).await;
    let id = testing::track_id("ar_store", 1);

    keep(&f.state, &f.account, &netease(&id)).await;

    let key = format!("tracks/{id}/high.flac");
    assert_eq!(f.objects.get(&key), Some(audio()));
    assert_eq!(
        row(&f.state, &id).await,
        Some(ledger::StoredTrack {
            platform: "netease".to_owned(),
            track_id: id.clone(),
            quality: "high".to_owned(),
            object_key: key,
            format: "flac".to_owned(),
            bit_rate: 999_000,
            bytes: 4096,
        })
    );
}

/// 同一首第二次播放不再下载。
#[tokio::test]
async fn a_stored_track_is_not_downloaded_again() {
    let f = fixture("ar_twice", false).await;
    let id = testing::track_id("ar_twice", 1);

    keep(&f.state, &f.account, &netease(&id)).await;
    keep(&f.state, &f.account, &netease(&id)).await;

    assert_eq!(f.hits.load(Ordering::SeqCst), 1);
}

/// 同一首同时报两次,也只下载一遍。
#[tokio::test]
async fn concurrent_plays_download_once() {
    let f = fixture("ar_race", false).await;
    let id = testing::track_id("ar_race", 1);
    let track = netease(&id);

    tokio::join!(
        keep(&f.state, &f.account, &track),
        keep(&f.state, &f.account, &track),
    );

    assert_eq!(f.hits.load(Ordering::SeqCst), 1);
}

/// 试听片段不存:存下来交出去就是一首放三十秒就停的歌。
#[tokio::test]
async fn a_trial_clip_is_not_stored() {
    let f = fixture("ar_trial", true).await;
    let id = testing::track_id("ar_trial", 1);

    keep(&f.state, &f.account, &netease(&id)).await;

    assert_eq!(f.hits.load(Ordering::SeqCst), 0);
    assert_eq!(row(&f.state, &id).await, None);
}

/// `/played` 照常 204,并在后台把这首存进去。
#[tokio::test]
async fn reporting_a_play_stores_the_track() {
    let f = fixture("ar_played", false).await;
    let id = testing::track_id("ar_played", 1);

    let status =
        crate::routes::library::history::record_play(
            State(f.state.clone()),
            f.account.clone(),
            Json(PlayedDto {
                platform: "netease".to_owned(),
                track_id: id.clone(),
            }),
        )
        .await
        .expect("起播上报应当成功");
    assert_eq!(status, axum::http::StatusCode::NO_CONTENT);

    let key = format!("tracks/{id}/high.flac");
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_secs(5);
    while f.objects.get(&key).is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "五秒内没存进去"
        );
        tokio::time::sleep(
            std::time::Duration::from_millis(20),
        )
        .await;
    }
}

/// 上游给不出源:什么都不存,也不 panic —— 用户那边照样在听。
#[tokio::test]
async fn an_upstream_failure_stores_nothing() {
    let f = fixture("ar_fail", false).await;
    let id = testing::track_id("ar_fail", 1);
    let state = testing::with_upstream(
        &f.state,
        testing::unreachable_upstream(),
    );

    keep(&state, &f.account, &netease(&id)).await;

    assert_eq!(row(&state, &id).await, None);
}

/// 一个此刻连不上的对象存储:每个动作都失败。
struct Unreachable;

impl server::objects::Objects for Unreachable {
    fn put(
        &self,
        _key: &str,
        _bytes: Vec<u8>,
        _content_type: &'static str,
    ) -> futures_util::future::BoxFuture<
        '_,
        server::objects::ObjectResult<()>,
    > {
        Box::pin(async { Err("连不上".to_owned()) })
    }

    fn exists(
        &self,
        _key: &str,
    ) -> futures_util::future::BoxFuture<
        '_,
        server::objects::ObjectResult<bool>,
    > {
        Box::pin(async { Err("连不上".to_owned()) })
    }

    fn delete(
        &self,
        _key: &str,
    ) -> futures_util::future::BoxFuture<
        '_,
        server::objects::ObjectResult<()>,
    > {
        Box::pin(async { Err("连不上".to_owned()) })
    }

    fn presign_get(&self, key: &str) -> String {
        format!("unreachable://{key}")
    }
}

/// `/play` 这首时交出的链接。
async fn play_url(
    state: &AppState,
    account: &Account,
    id: &str,
) -> String {
    crate::routes::play::play(
        State(state.clone()),
        account.clone(),
        axum::extract::Path(id.to_owned()),
    )
    .await
    .expect("取播放源应当成功")
    .0
    .url
}

/// 存过的歌从对象存储交付,不找上游 —— 上游此刻连不上也照样放得出来。
#[tokio::test]
async fn a_stored_track_plays_from_the_store() {
    let f = fixture("ar_serve", false).await;
    let id = testing::track_id("ar_serve", 1);
    keep(&f.state, &f.account, &netease(&id)).await;

    let offline = testing::with_upstream(
        &f.state,
        testing::unreachable_upstream(),
    );
    let source = crate::routes::play::play(
        State(offline),
        f.account.clone(),
        axum::extract::Path(id.clone()),
    )
    .await
    .expect("存过的歌不该需要上游")
    .0;

    assert_eq!(
        source,
        contract::PlaySourceDto {
            url: format!("memory://tracks/{id}/high.flac"),
            format: "flac".to_owned(),
            bit_rate: 999_000,
            trial: false,
        }
    );
}

/// 没存过的歌照旧拿网易云的直链。
#[tokio::test]
async fn an_unstored_track_plays_from_upstream() {
    let f = fixture("ar_fresh", false).await;
    let id = testing::track_id("ar_fresh", 1);

    let url = play_url(&f.state, &f.account, &id).await;

    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
}

/// 账上有、桶里没有:退回网易云,并把那一行删掉,好让下一次 `/played` 重新存。
#[tokio::test]
async fn a_lost_object_falls_back_and_forgets_the_row() {
    let f = fixture("ar_lost", false).await;
    let id = testing::track_id("ar_lost", 1);
    keep(&f.state, &f.account, &netease(&id)).await;
    f.objects.lose(&format!("tracks/{id}/high.flac"));

    let url = play_url(&f.state, &f.account, &id).await;

    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert_eq!(row(&f.state, &id).await, None);
}

/// 对象存储不可用:退回网易云,账留着 —— 它只是此刻问不到,不是丢了。
#[tokio::test]
async fn an_unreachable_store_falls_back_and_keeps_the_row()
{
    let f = fixture("ar_down", false).await;
    let id = testing::track_id("ar_down", 1);
    keep(&f.state, &f.account, &netease(&id)).await;
    let down = AppState {
        archive: Some(Archive::new(Arc::new(Unreachable))),
        ..f.state.clone()
    };

    let url = play_url(&down, &f.account, &id).await;

    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(row(&down, &id).await.is_some());
}
