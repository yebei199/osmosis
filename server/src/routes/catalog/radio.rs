//! 电台(#159):私人 FM 与心动模式,一条路由两种模式,都为了推新歌。
//!
//! 听过的一律丢掉(用户 2026-09-27 定):判据是这个账号的 `play_events`。
//! 丢完不够就再问平台,但一次请求最多问 [`MAX_PULLS`] 回 —— 过滤太狠时
//! 会连着问,而连着问平台就是风控的靶子。问满了仍不够,交出已有的。

use axum::{
    Json,
    extract::{Query, State},
};
use contract::{TrackDto, TracksDto};
use serde::Deserialize;

use server::bangdream::{
    self,
    proto::{
        GetIntelligenceListRequest, GetPersonalFmRequest,
        Platform,
    },
};
use server::error::{self, Failure};
use server::store::account::Account;
use server::store::history;
use server::store::playlist::TrackRef;

use super::search::remember_details;
use crate::routes::library::for_account;
use crate::routes::library::likes::netease_liked_id;
use crate::{AppState, conn, fail};

/// 一次请求最多问平台几回。
pub(crate) const MAX_PULLS: usize = 5;

/// 攒到这么多首就不再问。私人 FM 一批通常 3 首,丢掉任何一首就再问一回。
pub(crate) const ENOUGH: usize = 3;

/// 心动模式问平台要几首。平台不把它当上限(要 5 首回过 145 首),
/// 只是个必填字段,下游按实际返回量处理。
const HEART_COUNT: i32 = 50;

#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RadioMode {
    /// 网易云私人 FM。
    Fm,
    /// 心动模式:以 `seed` 这首为起点。
    Heart,
}

#[derive(Deserialize)]
pub(crate) struct RadioQuery {
    pub(crate) mode: RadioMode,
    /// 心动模式的种子曲目 id。私人 FM 不看它。
    pub(crate) seed: Option<String>,
}

/// 往平台要一批的办法。心动模式的歌单 id 只问一次,不随每回拉取重问。
enum Source {
    Fm,
    Heart { seed: String, playlist_id: String },
}

/// `GET /radio?mode=fm` / `GET /radio?mode=heart&seed=<曲目 id>` —— 一批没听过的新歌。
pub(crate) async fn radio(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<RadioQuery>,
) -> Result<Json<TracksDto>, Failure> {
    let source = match query.mode {
        RadioMode::Fm => Source::Fm,
        RadioMode::Heart => {
            let seed = query
                .seed
                .filter(|seed| !seed.is_empty())
                .ok_or_else(|| {
                    fail(&tonic::Status::invalid_argument(
                        "心动模式要一首种子曲目(seed)",
                    ))
                })?;
            Source::Heart {
                seed,
                playlist_id: netease_liked_id(
                    &state, &account,
                )
                .await?,
            }
        }
    };

    let mut picked: Vec<TrackDto> = Vec::new();
    for pull in 1..=MAX_PULLS {
        let batch = source.pull(&state, &account).await?;
        let fetched = batch.len();
        let fresh =
            unheard(&state, account.id, batch, &picked)
                .await?;
        tracing::info!(
            mode = ?query.mode,
            pull,
            fetched,
            dropped = fetched - fresh.len(),
            "电台拉了一批"
        );
        picked.extend(fresh);
        if picked.len() >= ENOUGH || fetched == 0 {
            break;
        }
    }
    remember_details(&state, &picked).await;

    // 电台区也是一个歌单视图,同样要分组筛选(#160)
    Ok(Json(
        for_account(
            &state,
            account.id,
            TracksDto {
                tracks: picked,
                unavailable: 0,
                hidden: 0,
            },
        )
        .await,
    ))
}

impl Source {
    async fn pull(
        &self,
        state: &AppState,
        account: &Account,
    ) -> Result<Vec<TrackDto>, Failure> {
        let mut discover = state.upstream.discover.clone();
        let tracks = match self {
            Self::Fm => {
                discover
                    .get_personal_fm(bangdream::as_user(
                        account,
                        GetPersonalFmRequest {
                            platform: Platform::Netease
                                as i32,
                        },
                    ))
                    .await
                    .map_err(|status| fail(&status))?
                    .into_inner()
                    .tracks
            }
            Self::Heart { seed, playlist_id } => {
                discover
                    .get_intelligence_list(
                        bangdream::as_user(
                            account,
                            GetIntelligenceListRequest {
                                platform: Platform::Netease
                                    as i32,
                                seed_track_id: seed.clone(),
                                playlist_id: playlist_id
                                    .clone(),
                                count: HEART_COUNT,
                            },
                        ),
                    )
                    .await
                    .map_err(|status| fail(&status))?
                    .into_inner()
                    .tracks
            }
        };

        Ok(tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect())
    }
}

/// `batch` 里没听过、也不在 `picked` 里的那些。同一批里重复的只留第一首。
async fn unheard(
    state: &AppState,
    account_id: i64,
    batch: Vec<TrackDto>,
    picked: &[TrackDto],
) -> Result<Vec<TrackDto>, Failure> {
    let refs: Vec<TrackRef> = batch
        .iter()
        .map(|track| TrackRef {
            platform: track.platform.clone(),
            track_id: track.id.clone(),
        })
        .collect();
    let mut conn = conn(&state.pool).await?;
    let played =
        history::played_among(&mut conn, account_id, &refs)
            .await
            .map_err(|err| error::map_error(&err))?;

    let mut fresh: Vec<TrackDto> = Vec::new();
    for (track, key) in batch.into_iter().zip(refs) {
        let seen = played.contains(&key)
            || picked
                .iter()
                .chain(&fresh)
                .any(|kept| same(kept, &key));
        if !seen {
            fresh.push(track);
        }
    }
    Ok(fresh)
}

fn same(track: &TrackDto, key: &TrackRef) -> bool {
    track.platform == key.platform
        && track.id == key.track_id
}

#[cfg(test)]
mod tests;
