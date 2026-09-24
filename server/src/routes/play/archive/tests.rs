//! 听过的歌存进对象存储:存、不重复存、试听不存、`/played` 真的会触发;
//! 再播时从对象存储交付,它出岔子时退回网易云;清理只删没人红心、三天没播的。

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

/// 摆好假上游、内存对象存储与账号;同一测试名留下的账目按 id 前缀清掉。
async fn fixture(case: &str, trial: bool) -> Fixture {
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    sqlx::query(
        "DELETE FROM stored_tracks WHERE track_id LIKE $1",
    )
    .bind(format!("{}-%", testing::scoped(case)))
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

/// 在事务里记一首存歌(连同桶里的对象),最后一次播放拨回 `days_ago` 天前。
async fn stored_days_ago(
    tx: &mut sqlx::PgConnection,
    objects: &MemoryObjects,
    id: &str,
    days_ago: i32,
) -> String {
    use server::objects::Objects as _;

    let key = format!("tracks/{id}/high.mp3");
    objects.put(&key, audio(), "audio/mpeg").await.unwrap();
    ledger::record(
        tx,
        &ledger::StoredTrack {
            platform: "netease".to_owned(),
            track_id: id.to_owned(),
            quality: quality(),
            object_key: key.clone(),
            format: "mp3".to_owned(),
            bit_rate: 320_000,
            bytes: 4096,
        },
    )
    .await
    .expect("记账失败");
    sqlx::query(
        "UPDATE stored_tracks
         SET last_played_at = now() - $2 * interval '1 day'
         WHERE track_id = $1",
    )
    .bind(id)
    .bind(days_ago)
    .execute(&mut *tx)
    .await
    .expect("拨时间失败");
    key
}

/// 把一首放进某个账号的红心(自家库里的那份缓存)。
async fn liked_by(
    tx: &mut sqlx::PgConnection,
    account: &Account,
    id: &str,
) {
    sqlx::query(
        "INSERT INTO platform_tracks (platform, track_id, title, artists, duration_ms)
         VALUES ('netease', $1, 't', ARRAY['a'], 1)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .expect("写详情失败");
    sqlx::query(
        "INSERT INTO platform_playlist_tracks
             (account_id, playlist_id, platform, track_id, position)
         VALUES ($1, $2, 'netease', $3, 0)",
    )
    .bind(account.id)
    .bind(server::store::cache::LIKED_PLAYLIST_ID)
    .bind(id)
    .execute(&mut *tx)
    .await
    .expect("写红心失败");
}

/// 清理只删「没人红心、且三天没播」的;红心的与三天内播过的都留着。
#[tokio::test]
async fn the_sweep_keeps_liked_and_recent_tracks() {
    let f = fixture("ar_sweep", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let old = testing::track_id("ar_sweep", 1);
    let liked = testing::track_id("ar_sweep", 2);
    let recent = testing::track_id("ar_sweep", 3);

    let old_key =
        stored_days_ago(&mut tx, &f.objects, &old, 4).await;
    let liked_key =
        stored_days_ago(&mut tx, &f.objects, &liked, 4)
            .await;
    liked_by(&mut tx, &f.account, &liked).await;
    let recent_key =
        stored_days_ago(&mut tx, &f.objects, &recent, 2)
            .await;

    super::sweep(&mut tx, f.objects.as_ref())
        .await
        .expect("清理应当成功");

    assert_eq!(f.objects.get(&old_key), None);
    assert!(f.objects.get(&liked_key).is_some());
    assert!(f.objects.get(&recent_key).is_some());
    let left = |id: &str| {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM stored_tracks WHERE track_id = $1",
        )
        .bind(id.to_owned())
    };
    assert_eq!(
        left(&old).fetch_one(&mut *tx).await.unwrap(),
        0
    );
    assert_eq!(
        left(&liked).fetch_one(&mut *tx).await.unwrap(),
        1
    );
    assert_eq!(
        left(&recent).fetch_one(&mut *tx).await.unwrap(),
        1
    );
}

/// 对象删不掉时那一行留着,下一轮再来 —— 先删行的话桶里就多一个没人记得的孤儿。
#[tokio::test]
async fn the_sweep_keeps_the_row_when_the_object_cannot_be_deleted()
 {
    let f = fixture("ar_orphan", false).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    let id = testing::track_id("ar_orphan", 1);
    stored_days_ago(&mut tx, &f.objects, &id, 4).await;

    super::sweep(&mut tx, &Unreachable)
        .await
        .expect("删不掉对象不算清理失败");

    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stored_tracks WHERE track_id = $1",
    )
    .bind(&id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(left, 1);
}

/// 取消红心从那一刻重新数三天,而不是按很久以前那次播放当场就删。
#[tokio::test]
async fn unliking_restarts_the_clock() {
    let f = fixture("ar_unlike", false).await;
    let id = testing::track_id("ar_unlike", 1);
    let mut conn = f.state.pool.acquire().await.unwrap();
    stored_days_ago(&mut conn, &f.objects, &id, 4).await;
    drop(conn);

    crate::routes::library::likes::unlike_track(
        State(f.state.clone()),
        f.account.clone(),
        axum::extract::Path(id.clone()),
    )
    .await
    .expect("取消红心应当成功");

    let mut conn = f.state.pool.acquire().await.unwrap();
    let expired = ledger::expired(&mut conn, super::RETAIN)
        .await
        .unwrap();
    assert!(
        expired.iter().all(|track| track.track_id != id),
        "刚取消红心的歌不该算过期"
    );
}
