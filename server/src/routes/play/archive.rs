//! 听过的歌存进对象存储(#126)。
//!
//! `/played` 报一次起播,这里就在后台把那首的音频按**上游原始格式**存进桶
//! (不转码,无损的留无损)。取源与拉流与 `/download` 是同一条路、同一个档位。
//!
//! 全程只记日志、不回报:存歌是顺手的事,它失败了用户照样在听,`/played`
//! 的响应也不等它。
//!
//! 再播时 [`stored_source`] 把桶里那份的签名链接交给 `/play`;对象存储出任何
//! 岔子都只是退回网易云,不让点歌失败。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use axum::Json;
use contract::PlaySourceDto;
use tokio::sync::Semaphore;

use server::objects::Objects;
use server::store::account::Account;
use server::store::archive::{self, StoredTrack};
use server::store::playlist::TrackRef;

use crate::AppState;
use crate::routes::play::PLAY_QUALITY;
use crate::routes::play::download;

#[cfg(test)]
mod tests;

/// 同时最多存几首。
///
/// 每一首整个读进内存再上传(无损的几十 MB),而且和正在播放的那首抢同一条
/// 出口带宽。两首足够跟上一个人切歌的速度,再多就只是在和播放抢资源。
const CONCURRENT_STORES: usize = 2;

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

/// 档位在键与表里的写法,如 `high`。
pub(crate) fn quality() -> String {
    PLAY_QUALITY
        .as_str_name()
        .trim_start_matches("QUALITY_LEVEL_")
        .to_ascii_lowercase()
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
    let quality = quality();
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
            &quality,
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
    )
    .await
    .map_err(describe)?;
    // 试听片段只有三十秒,存下来再交出去就是一首永远放不完的歌
    if source.trial {
        return Ok(());
    }
    let format = source.format.to_ascii_lowercase();
    let content_type = content_type(&format)?;
    let bytes = download::fetch(&source.url)
        .await
        .map_err(describe)?
        .bytes()
        .await
        .map_err(|err| err.to_string())?
        .to_vec();

    let stored = StoredTrack {
        object_key: format!(
            "tracks/{}/{quality}.{format}",
            track.track_id
        ),
        platform: track.platform.clone(),
        track_id: track.track_id.clone(),
        quality,
        format,
        bit_rate: source.bit_rate,
        bytes: i64::try_from(bytes.len())
            .unwrap_or(i64::MAX),
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
