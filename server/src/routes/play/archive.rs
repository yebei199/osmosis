//! 听过的歌存进对象存储(#126)。
//!
//! `/played` 报一次起播,这里就在后台把那首的音频按**上游原始格式**存进桶
//! (不转码)。取源与拉流与 `/download` 是同一条路,档位是 [`CACHE_TIER`]。
//!
//! 全程只记日志、不回报:存歌是顺手的事,它失败了用户照样在听,`/played`
//! 的响应也不等它。
//!
//! 再播时 [`stored_source`] 把桶里那份的签名链接交给 `/play`;对象存储出任何
//! 岔子都只是退回网易云,不让点歌失败。
//!
//! 只存无损:音源给不出无损的这首不存(`docs/adr/0034`)。
//!
//! 只有红心的歌长期留着:[`spawn_sweeper`] 每小时删一轮没人红心、且最后一次
//! 播放已满 [`RETAIN`] 的,以及 #147 之前按 320k 存的那批。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use contract::PlaySourceDto;
use sqlx::{PgConnection, PgPool};
use tokio::sync::Semaphore;

use server::objects::Objects;
use server::quality::{
    Quality, Tier, flac_stream_info, guess_tier, netease,
};
use server::store::account::Account;
use server::store::archive::{self, StoredTrack};
use server::store::playlist::TrackRef;

use crate::AppState;
use crate::routes::play::download;

#[cfg(test)]
mod tests;

/// 同时最多存几首。
///
/// 每一首整个读进内存再上传(无损的几十 MB),而且和正在播放的那首抢同一条
/// 出口带宽。两首足够跟上一个人切歌的速度,再多就只是在和播放抢资源。
const CONCURRENT_STORES: usize = 2;

/// 没人红心的歌,最后一次播放(或取消红心)之后留多久。用户定的三天(#126)。
pub(crate) const RETAIN: Duration =
    Duration::from_secs(3 * 24 * 3600);

/// 多久清一轮。
///
/// 保留期以天计,晚删一小时只多占三天的 1/72;而一轮只是一条走索引的查询
/// 加几次 DELETE,一小时一次对库与 RustFS 都可以忽略。
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

/// 目前唯一的平台。别的平台报上来的起播不存 —— 取源只认网易云。
const NETEASE: &str = "netease";

/// 对象存储,连同「同时存几首」「哪几首正在存」这两份进程内的状态。
#[derive(Clone)]
pub(crate) struct Archive {
    objects: Arc<dyn Objects>,
    slots: Arc<Semaphore>,
    /// 正在存的曲目。同一首连报两次 `/played` 时,第二次看到它在这里就走开,
    /// 不会同一首下载两遍。
    storing: Arc<Mutex<HashSet<String>>>,
}

impl Archive {
    pub(crate) fn new(objects: Arc<dyn Objects>) -> Self {
        Self {
            objects,
            slots: Arc::new(Semaphore::new(
                CONCURRENT_STORES,
            )),
            storing: Arc::default(),
        }
    }
}

/// 缓存向音源要的档位(#147,`docs/adr/0034`)。
pub(crate) const CACHE_TIER: Tier = Tier::Lossless;

/// 档位在键与表里的写法,如 `lossless`。
pub(crate) fn quality() -> String {
    CACHE_TIER.name().to_owned()
}

/// `/played` 之后调用:后台去存,立刻返回。没配对象存储就什么都不做。
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

/// 存一首。已经存过的只把「最后一次播放」拨到现在。
pub(crate) async fn keep(
    state: &AppState,
    account: &Account,
    track: &TrackRef,
) {
    let Some(archive) = &state.archive else {
        return;
    };
    if track.platform != NETEASE {
        return;
    }
    if !claim(archive, &track.track_id) {
        return;
    }

    let outcome =
        store(state, archive, account, track).await;
    release(archive, &track.track_id);

    if let Err(err) = outcome {
        tracing::warn!(
            track_id = %track.track_id,
            %err,
            "存进对象存储失败"
        );
    }
}

/// 占住这一首。已经有人在存就返回 `false`。
fn claim(archive: &Archive, track_id: &str) -> bool {
    archive
        .storing
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(track_id.to_owned())
}

fn release(archive: &Archive, track_id: &str) {
    archive
        .storing
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(track_id);
}

async fn store(
    state: &AppState,
    archive: &Archive,
    account: &Account,
    track: &TrackRef,
) -> Result<(), String> {
    let quality_key = quality();
    // 连接只借这一下:下载可能要几十秒,池里一共才五条
    {
        let mut conn = state
            .pool
            .acquire()
            .await
            .map_err(|err| err.to_string())?;
        if archive::touch(
            &mut conn,
            &track.platform,
            &track.track_id,
            &quality_key,
        )
        .await
        .map_err(|err| format!("{err:?}"))?
        {
            return Ok(());
        }
    }

    let _slot = archive
        .slots
        .acquire()
        .await
        .map_err(|err| err.to_string())?;

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
        return Ok(());
    }
    let mut quality = netease::quality_of(&source);
    // 给不出无损就不存:桶里只留无损(docs/adr/0034),这首每次播放现取
    if !quality.tier.is_lossless() {
        tracing::info!(
            track_id = %track.track_id,
            tier = quality.tier.name(),
            "音源给不出无损,不存"
        );
        return Ok(());
    }
    let content_type = content_type(&quality.format)?;
    let bytes = download::fetch(&source.url)
        .await
        .map_err(describe)?
        .bytes()
        .await
        .map_err(|err| err.to_string())?
        .to_vec();
    if let Some((bits, rate)) = flac_stream_info(&bytes) {
        quality.bits_per_sample = Some(bits);
        quality.sample_rate = Some(rate);
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
        bytes: i64::try_from(bytes.len())
            .unwrap_or(i64::MAX),
        tier: quality.tier.name().to_owned(),
        bits_per_sample: quality.bits_per_sample,
        sample_rate: quality.sample_rate,
    };
    archive
        .objects
        .put(&stored.object_key, bytes, content_type)
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
    Ok(())
}

/// 这首存过、对象也还在,就交出桶里那份的签名链接;否则 `None`,调用方去找网易云。
///
/// 每次都先问一句对象在不在(集群内一个 HEAD,毫秒级):账上有、桶里没有时
/// 交出去的链接必然 404,那就是一首点了没声的歌。账上有、桶里没有的那一行
/// 当场删掉,下一次 `/played` 会把它重新存回来。
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
