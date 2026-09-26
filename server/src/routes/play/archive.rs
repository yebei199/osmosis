//! 听过的歌存进对象存储(#126)。
//!
//! 曲目按**上游原始格式**存进桶(不转码),档位是 [`CACHE_TIER`]。取源与拉流与
//! `/download` 是同一条路。字节边从上游下、边往桶里传,不整首读进内存:无损单曲
//! 几十到上百 MB,整首缓冲在 512Mi 的 pod 里撑爆过(#147)。谁该存由预取队列定(#147,见 `prefetch`):我们的歌单、
//! 当天日推,以及 `/played` 报上来的还没存过的那首。
//!
//! 全程只记日志、不回报:存歌是顺手的事,它失败了用户照样在听。
//!
//! 再播时 [`stored_source`] 把桶里那份的签名链接交给 `/play`;对象存储出任何
//! 岔子都只是退回网易云,不让点歌失败。
//!
//! 只存无损:音源给不出无损的这首不存(`docs/adr/0034`)。
//!
//! 在我们任何一个歌单里、或在当天日推里的长期留着;[`spawn_sweeper`] 每小时删一轮
//! 哪都不在、且最后一次播放已满 [`RETAIN`] 的,以及 #147 之前按 320k 存的那批。
//! 桶有空间上限(默认 [`DEFAULT_CAP_BYTES`]),超过时按「我的喜欢 → 日推 →
//! 其他歌单 → 哪都不在」取舍,存不下的记日志,见 [`make_room`]。

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use contract::PlaySourceDto;
use futures_util::{StreamExt, TryStreamExt};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};

use server::objects::{ObjectBody, Objects, whole};
use server::quality::{
    Quality, Tier, flac_stream_info, guess_tier, netease,
};
use server::store::account::Account;
use server::store::archive::{self, StoredTrack};
use server::store::playlist::TrackRef;
use server::store::prefetch as queue;

use crate::routes::play::{download, prefetch};
use crate::{AppState, conn};

#[cfg(test)]
mod tests;

/// 没人红心的歌,最后一次播放(或取消红心)之后留多久。用户定的三天(#126)。
pub(crate) const RETAIN: Duration =
    Duration::from_secs(3 * 24 * 3600);

/// 多久清一轮。
///
/// 保留期以天计,晚删一小时只多占三天的 1/72;而一轮只是一条走索引的查询
/// 加几次 DELETE,一小时一次对库与 RustFS 都可以忽略。
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

/// 目前唯一的平台。别的平台的曲目不存 —— 取源只认网易云。
pub(crate) const NETEASE: &str = "netease";

/// 桶的空间上限的默认值:85 GB(十进制)。RustFS 的数据卷是 98 GB,留十几 GB 余量。
pub(crate) const DEFAULT_CAP_BYTES: i64 = 85_000_000_000;

/// 对象存储,连同它的空间上限。同时存几首、多快去问音源,由预取队列的 worker 管
/// (见 `prefetch`)。
#[derive(Clone)]
pub(crate) struct Archive {
    objects: Arc<dyn Objects>,
    /// 桶里最多放多少字节(#147)。超过时按名次取舍,见 [`make_room`]。
    cap_bytes: i64,
}

impl Archive {
    pub(crate) fn new(objects: Arc<dyn Objects>) -> Self {
        Self {
            objects,
            cap_bytes: DEFAULT_CAP_BYTES,
        }
    }

    pub(crate) fn with_cap(self, cap_bytes: i64) -> Self {
        Self { cap_bytes, ..self }
    }
}

/// 缓存向音源要的档位(#147,`docs/adr/0034`)。
pub(crate) const CACHE_TIER: Tier = Tier::Lossless;

/// 档位在键与表里的写法,如 `lossless`。
pub(crate) fn quality() -> String {
    CACHE_TIER.name().to_owned()
}

/// 一次存的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stored {
    /// 这一次存进去了。
    Now,
    /// 早就在桶里,没再下载。
    Already,
    /// 音源给不出无损(或只给试听片段),不存。
    NoLossless,
    /// 放进去会超出空间上限,排在它后面的又腾不出地方,不存。
    OverCap,
}

/// `/played` 之后调用:后台去办,立刻返回。没配对象存储就什么都不做。
pub(crate) fn spawn_keep(
    state: &AppState,
    account: Account,
    track: TrackRef,
) {
    if state.archive.is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        keep(&state, &account, &track).await;
    });
}

/// 播过一首:存过的把「最后一次播放」拨到现在,没存过的排进预取队列。
pub(crate) async fn keep(
    state: &AppState,
    account: &Account,
    track: &TrackRef,
) {
    if state.archive.is_none() || track.platform != NETEASE
    {
        return;
    }
    let touched = match state.pool.acquire().await {
        Ok(mut conn) => archive::touch(
            &mut conn,
            &track.platform,
            &track.track_id,
            &quality(),
        )
        .await
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    match touched {
        Ok(true) => {}
        Ok(false) => {
            prefetch::enqueue(
                state,
                account.id,
                std::slice::from_ref(track),
            )
            .await;
        }
        Err(err) => {
            tracing::warn!(track_id = %track.track_id, %err, "查不了存歌的账")
        }
    }
}

/// 以这个账号的凭据把一首存进桶。预取 worker 调它;没配对象存储是错误。
pub(crate) async fn store_track(
    state: &AppState,
    account: &Account,
    track: &TrackRef,
) -> Result<Stored, String> {
    let archive = state
        .archive
        .as_ref()
        .ok_or_else(|| "没配对象存储".to_owned())?;
    if track.platform != NETEASE {
        return Err(format!(
            "取不了 {} 的源",
            track.platform
        ));
    }
    store(state, archive, account, track).await
}

async fn store(
    state: &AppState,
    archive: &Archive,
    account: &Account,
    track: &TrackRef,
) -> Result<Stored, String> {
    let quality_key = quality();
    // 连接只借这一下:下载可能要几十秒,池里一共才五条
    {
        let mut conn = state
            .pool
            .acquire()
            .await
            .map_err(|err| err.to_string())?;
        if archive::find(
            &mut conn,
            &track.platform,
            &track.track_id,
            &quality_key,
        )
        .await
        .map_err(|err| format!("{err:?}"))?
        .is_some()
        {
            return Ok(Stored::Already);
        }
    }

    let source = download::play_source(
        state,
        account,
        &track.track_id,
        CACHE_TIER,
    )
    .await
    .map_err(describe)?;
    // 试听片段只有三十秒,存下来再交出去就是一首永远放不完的歌
    if source.trial {
        return Ok(Stored::NoLossless);
    }
    let mut quality = netease::quality_of(&source);
    // 给不出无损就不存:桶里只留无损(docs/adr/0034),这首每次播放现取
    if !quality.tier.is_lossless() {
        tracing::info!(
            track_id = %track.track_id,
            tier = quality.tier.name(),
            "音源给不出无损,不存"
        );
        return Ok(Stored::NoLossless);
    }
    let content_type = content_type(&quality.format)?;
    let upstream = download::fetch(&source.url)
        .await
        .map_err(describe)?;
    // 单个 PUT 要事先知道长度;不给长度的上游这次不存,队列会再排它
    let length =
        upstream.content_length().ok_or_else(|| {
            "上游没给 Content-Length,流式存不了".to_owned()
        })?;
    let mut body: ObjectBody = upstream
        .bytes_stream()
        .map_err(std::io::Error::other)
        .boxed();
    let head = read_head(&mut body).await?;
    if let Some((bits, rate)) = flac_stream_info(&head) {
        quality.bits_per_sample = Some(bits);
        quality.sample_rate = Some(rate);
    }
    let size = i64::try_from(length).unwrap_or(i64::MAX);
    if !make_space(state, archive, track, size).await? {
        return Ok(Stored::OverCap);
    }

    let stored = StoredTrack {
        object_key: format!(
            "tracks/{}/{quality_key}.{}",
            track.track_id, quality.format
        ),
        platform: track.platform.clone(),
        track_id: track.track_id.clone(),
        quality: quality_key,
        format: quality.format,
        bit_rate: quality.bit_rate,
        bytes: size,
        tier: quality.tier.name().to_owned(),
        bits_per_sample: quality.bits_per_sample,
        sample_rate: quality.sample_rate,
    };
    archive
        .objects
        .put(
            &stored.object_key,
            // 读头时取走的那几块接回流的开头
            whole(head).chain(body).boxed(),
            length,
            content_type,
        )
        .await?;

    let mut conn = state
        .pool
        .acquire()
        .await
        .map_err(|err| err.to_string())?;
    archive::record(&mut conn, &stored)
        .await
        .map_err(|err| format!("{err:?}"))?;

    tracing::info!(
        track_id = %stored.track_id,
        bytes = stored.bytes,
        format = %stored.format,
        tier = %stored.tier,
        "已存进对象存储"
    );
    Ok(Stored::Now)
}

/// FLAC 的 `fLaC`、块头与 STREAMINFO 一共这么长,位深与采样率都在里面。
const FLAC_HEAD: usize = 42;

/// 从流的开头攒出至少 [`FLAC_HEAD`] 字节(流更短就是全部)。只多读到凑够的那一块。
async fn read_head(
    body: &mut ObjectBody,
) -> Result<Vec<u8>, String> {
    let mut head = Vec::new();
    while head.len() < FLAC_HEAD {
        let Some(chunk) = body.next().await else {
            break;
        };
        head.extend_from_slice(
            &chunk.map_err(|err| err.to_string())?,
        );
    }
    Ok(head)
}

/// 放不放得下这 `size` 字节:放得下直接放;放不下就请名次在它后面的让位
/// (删对象、删账,在歌单或日推里的记一笔 over_cap),腾不出来就不存。
///
// ponytail: 几个 worker 各自先量后放,同时放的那几首可能一起越过上限,
// 最多越过 worker 数 × 一首的大小(百来 MB);要严丝合缝就把量与放包进一把 advisory lock。
async fn make_space(
    state: &AppState,
    archive: &Archive,
    track: &TrackRef,
    size: i64,
) -> Result<bool, String> {
    let mut conn = state
        .pool
        .acquire()
        .await
        .map_err(|err| err.to_string())?;
    let used = archive::total_bytes(&mut conn)
        .await
        .map_err(|err| format!("{err:?}"))?;
    if used.saturating_add(size) <= archive.cap_bytes {
        return Ok(true);
    }

    let rank = archive::rank(
        &mut conn,
        &track.platform,
        &track.track_id,
    )
    .await
    .map_err(|err| format!("{err:?}"))?;
    let behind = archive::yielding_to(&mut conn, rank)
        .await
        .map_err(|err| format!("{err:?}"))?;
    let sizes: Vec<i64> =
        behind.iter().map(|victim| victim.bytes).collect();
    let Some(count) =
        make_room(used, size, archive.cap_bytes, &sizes)
    else {
        tracing::warn!(
            track_id = %track.track_id,
            rank,
            used,
            size,
            cap = archive.cap_bytes,
            "超出空间上限,不存"
        );
        return Ok(false);
    };

    for victim in &behind[..count] {
        archive.objects.delete(&victim.object_key).await?;
        archive::forget(&mut conn, victim)
            .await
            .map_err(|err| format!("{err:?}"))?;
        queue::mark_evicted(
            &mut conn,
            &TrackRef {
                platform: victim.platform.clone(),
                track_id: victim.track_id.clone(),
            },
        )
        .await
        .map_err(|err| format!("{err:?}"))?;
        tracing::warn!(
            track_id = %victim.track_id,
            bytes = victim.bytes,
            for_track = %track.track_id,
            "超出空间上限,让位给排在前面的"
        );
    }
    Ok(true)
}

/// 已用 `used` 字节、上限 `cap`,要放进 `incoming` 字节:按顺序请 `candidates`
/// (各自的字节数)让位,要让掉前几个才放得下。全让掉也放不下是 `None`。
pub(crate) fn make_room(
    used: i64,
    incoming: i64,
    cap: i64,
    candidates: &[i64],
) -> Option<usize> {
    let mut need = used.saturating_add(incoming) - cap;
    if need <= 0 {
        return Some(0);
    }
    for (index, bytes) in candidates.iter().enumerate() {
        need -= bytes;
        if need <= 0 {
            return Some(index + 1);
        }
    }
    None
}

/// 这首存过、对象也还在,就交出桶里那份的签名链接;否则 `None`,调用方去找网易云。
///
/// 每次都先问一句对象在不在(集群内一个 HEAD,毫秒级):账上有、桶里没有时
/// 交出去的链接必然 404,那就是一首点了没声的歌。账上有、桶里没有的那一行
/// 当场删掉,下一次 `/played` 会把它重新排进队列。
pub(crate) async fn stored_source(
    state: &AppState,
    track_id: &str,
) -> Option<PlaySourceDto> {
    let archive = state.archive.as_ref()?;
    let mut conn = state
        .pool
        .acquire()
        .await
        .inspect_err(
            |err| tracing::warn!(%err, "查不了存歌的账"),
        )
        .ok()?;
    let stored = archive::find(
        &mut conn,
        NETEASE,
        track_id,
        &quality(),
    )
    .await
    .inspect_err(|err| {
        tracing::warn!(?err, "查不了存歌的账")
    })
    .ok()??;

    match archive.objects.exists(&stored.object_key).await {
        Ok(true) => {
            tracing::info!(track_id, "从对象存储交付");
            Some(PlaySourceDto {
                url: archive
                    .objects
                    .presign_get(&stored.object_key),
                quality: Some(
                    stored_quality(&stored).to_dto(),
                ),
                format: stored.format,
                bit_rate: stored.bit_rate,
                trial: false,
            })
        }
        Ok(false) => {
            tracing::warn!(track_id, key = %stored.object_key, "账上有、桶里没有,删账退回网易云");
            if let Err(err) =
                archive::forget(&mut conn, &stored).await
            {
                tracing::warn!(?err, "删不掉那一行账");
            }
            None
        }
        Err(err) => {
            tracing::warn!(track_id, %err, "对象存储不可用,退回网易云");
            None
        }
    }
}

/// 账上记的实际音质。档位名认不得(不该发生)时按格式与码率认。
fn stored_quality(stored: &StoredTrack) -> Quality {
    Quality {
        tier: Tier::from_name(&stored.tier).unwrap_or_else(
            || guess_tier(&stored.format, stored.bit_rate),
        ),
        format: stored.format.clone(),
        bit_rate: stored.bit_rate,
        bits_per_sample: stored.bits_per_sample,
        sample_rate: stored.sample_rate,
    }
}

/// 取消红心那一刻起重新数 [`RETAIN`]:不然按很久以前那次播放算,当场就删了。
///
/// 失败只记日志 —— 取消红心本身已经在平台那边办成了。
pub(crate) async fn restart_clock(
    state: &AppState,
    track_id: &str,
) {
    if state.archive.is_none() {
        return;
    }
    let touched = match state.pool.acquire().await {
        Ok(mut conn) => archive::touch(
            &mut conn,
            NETEASE,
            track_id,
            &quality(),
        )
        .await
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    if let Err(err) = touched {
        tracing::warn!(track_id, %err, "取消红心后没能重新起算保留期");
    }
}

/// 清一轮:先删对象、再删那一行。返回删了几首。
///
/// 不是按 [`CACHE_TIER`] 要来的(#147 之前的 320k)不论保留期一并清掉。
///
/// 顺序不能反:先删行的话,对象删失败就再也没有人记得它,桶里多一个孤儿。
/// 对象删不掉的这一轮跳过,行留着,下一轮再来。
pub(crate) async fn sweep(
    conn: &mut PgConnection,
    objects: &dyn Objects,
) -> Result<usize, String> {
    let expired =
        archive::expired(conn, RETAIN, &quality())
            .await
            .map_err(|err| format!("{err:?}"))?;

    let mut removed = 0;
    for track in expired {
        if let Err(err) =
            objects.delete(&track.object_key).await
        {
            tracing::warn!(key = %track.object_key, %err, "删不掉过期的对象,下一轮再来");
            continue;
        }
        archive::forget_if_expired(
            conn,
            &track,
            RETAIN,
            &quality(),
        )
        .await
        .map_err(|err| format!("{err:?}"))?;
        removed += 1;
    }
    Ok(removed)
}

/// 启动时清一轮,之后每 [`SWEEP_EVERY`] 一轮。没配对象存储就不起。
pub(crate) fn spawn_sweeper(
    pool: PgPool,
    archive: &Archive,
) {
    let objects = archive.objects.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(SWEEP_EVERY);
        loop {
            // 第一次 tick 立刻就绪:进程起来就清一轮,不必先等一小时
            every.tick().await;
            let outcome = match pool.acquire().await {
                Ok(mut conn) => {
                    sweep(&mut conn, objects.as_ref()).await
                }
                Err(err) => Err(err.to_string()),
            };
            match outcome {
                Ok(0) => {}
                Ok(removed) => tracing::info!(
                    removed,
                    "清掉了过期的存歌"
                ),
                Err(err) => {
                    tracing::warn!(%err, "清理存歌失败")
                }
            }
        }
    });
}

/// `GET /archive/stats` 的响应:桶里存了多少、队列里还剩多少(#147)。
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ArchiveStats {
    /// 以无损存进桶的曲目数。
    pub(crate) tracks: i64,
    /// 它们一共多少字节。应当等于桶的用量。
    pub(crate) bytes: i64,
    /// 空间上限。没配对象存储时是 0。
    pub(crate) cap_bytes: i64,
    /// 预取队列里还在排的(含正在取的)。
    pub(crate) queued: i64,
    /// 因为超出空间上限没存的。
    pub(crate) over_cap: i64,
    /// 音源给不出无损的。
    pub(crate) no_lossless: i64,
    /// 重试用尽的。
    pub(crate) failed: i64,
}

/// `GET /archive/stats` —— 缓存的统计。界面上没有入口,带登录 token 用 curl 打它。
pub(crate) async fn stats(
    State(state): State<AppState>,
    _account: Account,
) -> Result<Json<ArchiveStats>, server::error::Failure> {
    let map = |err: server::error::AppError| {
        server::error::map_error(&err)
    };
    let mut conn = conn(&state.pool).await?;
    let (tracks, bytes) =
        archive::usage(&mut conn, &quality())
            .await
            .map_err(map)?;
    let counts =
        queue::counts(&mut conn).await.map_err(map)?;
    let count = |wanted: &str| {
        counts
            .iter()
            .find(|(state, _)| state == wanted)
            .map_or(0, |(_, n)| *n)
    };

    Ok(Json(ArchiveStats {
        tracks,
        bytes,
        cap_bytes: state
            .archive
            .as_ref()
            .map_or(0, |archive| archive.cap_bytes),
        queued: count("queued"),
        over_cap: count("over_cap"),
        no_lossless: count("no_lossless"),
        failed: count("failed"),
    }))
}

/// 上游给的格式进了对象键,也决定交回去的 `Content-Type`。
///
/// 只认见过的几种:它是外来的字符串,认不得的不拼进键里,这首就不存。
fn content_type(
    format: &str,
) -> Result<&'static str, String> {
    match format {
        "mp3" => Ok("audio/mpeg"),
        "flac" => Ok("audio/flac"),
        "m4a" | "aac" => Ok("audio/mp4"),
        "ogg" => Ok("audio/ogg"),
        other => Err(format!("不认得的音频格式 {other:?}")),
    }
}

/// 路由用的失败形状翻成一句日志。
fn describe(
    (status, Json(body)): server::error::Failure,
) -> String {
    format!("{status}: {}", body.message)
}
