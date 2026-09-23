//! 平台曲目的缓存回填:先读缓存,缺的按批向上游补,补完写回。
//!
//! 红心与歌单两条路都要用它,所以它不跟着任何一条走。

use contract::{TrackDto, TracksDto};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use server::bangdream::{
    self,
    proto::{
        GetPlaylistResponse, GetTracksRequest, Platform,
    },
};
use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::cache;

use tokio::task::{JoinError, JoinHandle};
use tracing::Instrument;

use crate::{AppState, conn, fail};

/// 先回库,到时候了再在后台回源(#124)。
///
/// `fetch` 是这个歌单完整的回源路径,最后一步必须经过 [`cached_tracks`] ——
/// 「多新」在那里记下。按库里那份的新旧分四种:
///
/// - 距上一次回源不到 [`REFRESH_EVERY`]:只答库里那份;
/// - 不到 [`MAX_AGE`]:答库里那份,后台回源,下一次打开看到结果;
/// - 更旧,或进程重启过不知道多新:回源最多等 [`FRESH_WAIT`],等不到先答库里那份;
/// - 库里没有:只能等回源。
///
/// 回源一律 spawn 出去:客户端等不及断开、这个请求被丢掉,它照样跑完并落库,
/// 下一次就命中。生产上一次回源 13 秒多而客户端 10 秒放弃,不这样的话
/// 库里永远填不上。回源失败就忘掉这份,下一次打开再等一回,让失败被看见。
pub(crate) async fn store_first<F, Fut>(
    state: &AppState,
    account: &Account,
    playlist_id: &str,
    fetch: F,
) -> Result<TracksDto, Failure>
where
    F: FnOnce(AppState, Account) -> Fut,
    Fut: Future<Output = Result<TracksDto, Failure>>
        + Send
        + 'static,
{
    let record =
        state.playlists.get(account.id, playlist_id);
    let unavailable = record.map_or(0, |(_, n)| n);
    if record
        .is_some_and(|(at, _)| at.elapsed() < REFRESH_EVERY)
    {
        return stored(
            state,
            account,
            playlist_id,
            unavailable,
        )
        .await;
    }

    let mut job =
        spawn_fetch(state, account, playlist_id, fetch);
    let stored =
        stored(state, account, playlist_id, unavailable)
            .await?;
    if stored.tracks.is_empty() {
        return joined(job.await);
    }
    if record.is_some_and(|(at, _)| at.elapsed() < MAX_AGE)
    {
        return Ok(stored);
    }

    match tokio::time::timeout(FRESH_WAIT, &mut job).await {
        Ok(done) => joined(done),
        Err(_) => Ok(stored),
    }
}

/// 库里这个歌单现有的那份。
async fn stored(
    state: &AppState,
    account: &Account,
    playlist_id: &str,
    unavailable: usize,
) -> Result<TracksDto, Failure> {
    let mut conn = conn(&state.pool).await?;
    let tracks = cache::tracks_of(
        &mut conn,
        account.id,
        playlist_id,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(TracksDto {
        tracks,
        unavailable,
    })
}

/// 把回源放进独立任务,不随请求一起被丢掉。
fn spawn_fetch<F, Fut>(
    state: &AppState,
    account: &Account,
    playlist_id: &str,
    fetch: F,
) -> JoinHandle<Result<TracksDto, Failure>>
where
    F: FnOnce(AppState, Account) -> Fut,
    Fut: Future<Output = Result<TracksDto, Failure>>
        + Send
        + 'static,
{
    let playlists = state.playlists.clone();
    let (account_id, playlist_id) =
        (account.id, playlist_id.to_owned());
    let refresh = fetch(state.clone(), account.clone());

    // 带上当前的 req span:后台那次的 upstream ms= 仍记在这个请求名下
    tokio::spawn(
        async move {
            let result = refresh.await;
            if let Err((status, body)) = &result {
                tracing::warn!(
                    %status,
                    code = body.code,
                    "回源失败,下一次打开再当场回源"
                );
                playlists.forget(account_id, &playlist_id);
            }
            result
        }
        .in_current_span(),
    )
}

fn joined(
    done: Result<Result<TracksDto, Failure>, JoinError>,
) -> Result<TracksDto, Failure> {
    done.unwrap_or_else(|err| {
        Err(fail(&tonic::Status::internal(format!(
            "回源任务没跑完: {err}"
        ))))
    })
}

/// 一次向上游要多少首曲目详情。
///
/// 973 首的歌单一次要不完 —— 上游把这些 id 拼进一个请求体发给平台,而平台对
/// 请求大小有自己的想法。分批只在**冷启动**发生:详情缓存下来之后,常态是
/// 一批都不用要。
pub(crate) const DETAIL_BATCH: usize = 200;

/// 库里那份多久以内先回库、不等上游。
///
/// 24 小时:天天开的人永远走快的那条路;隔了一天以上再开,平台那边多半已经
/// 变了不少(在手机官方 App 里点的心、别人往收藏歌单里加的歌),那一次宁可
/// 慢一秒也给新的。这也是 `docs/adr/0018` 说的「过期的上界」。
pub(crate) const MAX_AGE: Duration =
    Duration::from_secs(24 * 60 * 60);

/// 库里那份过期(或进程刚重启、不知道它多新)时,当场回源最多等多久。
///
/// 3 秒:本机一次回源约 1.3 秒,等得到就给新的;生产上慢时 13 秒多,
/// 而客户端 10 秒就放弃 —— 等不到就先给库里那份,回源在后台跑完。
pub(crate) const FRESH_WAIT: Duration =
    Duration::from_secs(3);

/// 先回库之后,距上一次回源至少这么久才在后台再回源一次。
///
/// 30 秒:进出同一个歌单、来回切页是秒级的动作,每一下都回源的话 978 首的
/// 红心每次是一秒的上游调用加一次比对,而这期间平台那边几乎不会变。
/// 比这长则手机上刚点的心要多等 —— 进出一次歌单超过 30 秒就能看到。
pub(crate) const REFRESH_EVERY: Duration =
    Duration::from_secs(30);

/// `(账号, 平台歌单)` → (最近一次回源成功的时刻, 那次给不出详情的曲目数)。
type Records = HashMap<(i64, String), (Instant, usize)>;

/// 每个 `(账号, 平台歌单)` 最近一次回源成功的时刻,和那次平台给不出详情的曲目数。
///
/// 只在内存里:进程重启就全忘,每个歌单第一次打开当场回源 —— 缓存整张删掉
/// 只是慢一次(`docs/adr/0018` 第 3 条),忘掉「多新」同理。放进库里就要
/// 加迁移,而本机开发库是几棵 worktree 共用的:旧代码的服务端遇到没见过的
/// 迁移会拒绝启动。
///
/// 「有没有缓存」也以这里为准,而不是库里有没有行:空歌单在库里同样是零行。
#[derive(Clone, Default)]
pub(crate) struct Freshness(Arc<Mutex<Records>>);

impl Freshness {
    fn lock(&self) -> std::sync::MutexGuard<'_, Records> {
        // 锁里只有 HashMap 的一次读写,不会在持锁时 panic;真毒化了也照用
        self.0.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn get(
        &self,
        account_id: i64,
        playlist_id: &str,
    ) -> Option<(Instant, usize)> {
        self.lock()
            .get(&(account_id, playlist_id.to_owned()))
            .copied()
    }

    fn record(
        &self,
        account_id: i64,
        playlist_id: &str,
        unavailable: usize,
    ) {
        self.lock().insert(
            (account_id, playlist_id.to_owned()),
            (Instant::now(), unavailable),
        );
    }

    /// 忘掉这一份:下一次打开当场回源。
    pub(crate) fn forget(
        &self,
        account_id: i64,
        playlist_id: &str,
    ) {
        self.lock()
            .remove(&(account_id, playlist_id.to_owned()));
    }

    /// 把记录往回拨 `by`,测试用来模拟「过了这么久」。
    #[cfg(test)]
    pub(crate) fn age(
        &self,
        account_id: i64,
        playlist_id: &str,
        by: Duration,
    ) {
        if let Some((at, _)) = self
            .lock()
            .get_mut(&(account_id, playlist_id.to_owned()))
        {
            *at -= by;
        }
    }
}

/// 把一个平台歌单的曲目备齐,并按平台给的次序读出来。
///
/// 只向平台要**缺详情**的那些:歌单的成员关系天天变(点一次红心就变一次),
/// 而曲目详情几乎不变,且跨歌单共用 —— 收藏的歌单里的歌大半已经在红心里了。
/// 每次都全量重取的话,这个缓存等于没有(见 `docs/adr/0018`)。
///
/// 平台不肯给详情的 id(下架、无权限)会被剔出成员关系:留着它只会在读回时
/// 的 JOIN 里消失,那时歌单少一首而没有任何人报错。
pub(crate) async fn cached_tracks(
    state: &AppState,
    account: &Account,
    playlist_id: &str,
    refs: &[cache::TrackRef],
    detail_tracks: &[TrackDto],
) -> Result<(Vec<TrackDto>, usize), Failure> {
    let mut conn = conn(&state.pool).await?;
    let platform = netease_name();

    // 歌单详情随手带回来的那一批先入库,它们不必再问平台要一遍。
    // 平台把这一批截断时,只有差额才走补拉。
    cache::put_details(&mut conn, detail_tracks)
        .await
        .map_err(|err| error::map_error(&err))?;
    let missing =
        bangdream::refs_missing_from(detail_tracks, refs);

    let unavailable =
        fill_details(state, account, &mut conn, &missing)
            .await?;
    // 剔掉平台给不出详情的那些,并记下剔了几条 —— 静默变短的歌单没人报得出来
    let (known, dropped) =
        bangdream::keep_available(refs, &unavailable);
    if dropped > 0 {
        tracing::warn!(
            playlist_id,
            dropped,
            "平台给不出详情,这些曲目没能进成员关系"
        );
    }

    cache::set_membership(
        &mut conn,
        account.id,
        playlist_id,
        &platform,
        &known,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    let tracks = cache::tracks_of(
        &mut conn,
        account.id,
        playlist_id,
    )
    .await
    .map_err(|err| error::map_error(&err))?;
    state.playlists.record(
        account.id,
        playlist_id,
        dropped,
    );

    Ok((tracks, dropped))
}

/// 把这些 id 里还缺的详情向平台要回来存下,返回平台**仍然给不出**的那些。
///
/// 分成两步问「谁还缺」是有意的:第一次问的是「要不要发请求」,第二次问的是
/// 「发完了还差谁」。合成一次的话,平台跳过的那些(下架、无权限)与从没问过的
/// 那些混在一起,分不出来。
pub(crate) async fn fill_details(
    state: &AppState,
    account: &Account,
    conn: &mut sqlx::PgConnection,
    ids: &[String],
) -> Result<HashSet<String>, Failure> {
    let platform = netease_name();

    let missing =
        cache::missing_details(conn, &platform, ids)
            .await
            .map_err(|err| error::map_error(&err))?;
    // 命中多少决定了这次要不要回源:全中是零次上游调用,冷启动是
    // ids / DETAIL_BATCH 次(#121)。
    tracing::info!(
        ids = ids.len(),
        missing = missing.len(),
        "详情缓存"
    );

    let mut catalog = state.upstream.catalog.clone();
    for chunk in missing.chunks(DETAIL_BATCH) {
        let response = catalog
            .get_tracks(bangdream::as_user(
                account,
                GetTracksRequest {
                    platform: Platform::Netease as i32,
                    track_ids: chunk.to_vec(),
                },
            ))
            .await
            .map_err(|status| fail(&status))?
            .into_inner();

        let tracks: Vec<TrackDto> = response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect();

        cache::put_details(conn, &tracks)
            .await
            .map_err(|err| error::map_error(&err))?;
    }

    Ok(cache::missing_details(conn, &platform, ids)
        .await
        .map_err(|err| error::map_error(&err))?
        .into_iter()
        .collect())
}

/// 缓存里代表网易云的那个字符串。
///
/// 走 `track_to_dto` 用的同一个函数 —— 另写一份的话,prost 生成的
/// `as_str_name()` 给的是 `PLATFORM_NETEASE`,与存进去的 `netease` 对不上,
/// 而那是运行期才炸的外键错误,编译器一声不吭。
pub(crate) fn netease_name() -> String {
    bangdream::platform_name(Platform::Netease as i32)
}

/// 歌单详情随手带回来的那一批曲目详情。
///
/// **会被平台截断**,所以它只是省往返的顺风车,不是全量 —— 歌单有多长的判据
/// 永远是 `track_refs`。差额由 `bangdream::refs_missing_from` 挑出来补拉。
pub(crate) fn detail_tracks_of(
    detail: &GetPlaylistResponse,
) -> Vec<TrackDto> {
    detail
        .tracks
        .iter()
        .cloned()
        .map(bangdream::track_to_dto)
        .collect()
}

/// 歌单详情里的成员关系。
pub(crate) fn track_refs_of(
    detail: &GetPlaylistResponse,
) -> Vec<cache::TrackRef> {
    detail
        .track_refs
        .iter()
        .map(|track| {
            cache::TrackRef::new(
                &track.id,
                Some(track.added_at_ms),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests;
