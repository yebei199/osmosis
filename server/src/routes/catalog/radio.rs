//! 电台(#159):私人 FM 与心动模式,一条路由两种模式,都为了推新歌。
//!
//! 听过的一律丢掉(用户 2026-09-27 定):判据是这个账号的 `play_events`;
//! 红心过、赞踩过、命中屏蔽规则的同样丢掉(2026-09-28 并进 #161)。
//! 丢完不够就再问平台,但一次请求最多问 [`MAX_PULLS`] 回 —— 过滤太狠时
//! 会连着问,而连着问平台就是风控的靶子。问满了仍不够,交出已有的。
//!
//! 带着筛选来的(#166,电台区选了 chip 再点歌)在排除之后再按筛选过一道;
//! 筛选越窄丢得越多,上限放到 [`MAX_FILTERED_PULLS`]。筛选看歌词的话,
//! 新歌多半还没探过,当场探一次(见 [`probe_lyrics`]),不等后台队列。

use axum::{
    Json,
    extract::{Query, State},
};
use contract::{
    FacetDto, FacetPickDto, TrackDto, TracksDto,
};
use futures_util::StreamExt;
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
use server::store::lyric::{self, LyricKind};
use server::store::playlist::TrackRef;

use super::lyric_probe;
use super::search::remember_details;
use crate::routes::library::for_account;
use crate::routes::library::likes::netease_liked_id;
use crate::{AppState, conn, fail};

/// 一次请求最多问平台几回。
pub(crate) const MAX_PULLS: usize = 5;

/// 带筛选时最多问几回。估的值(#166),按风控实测再调。
pub(crate) const MAX_FILTERED_PULLS: usize = 10;

/// 一次请求最多当场探几首的歌词。探歌词也是问平台,同样得有个顶。
// ponytail: 按请求计的顶,并发电台多了要换成跨请求的限速(像后台 worker 那样)
const MAX_PROBES: usize = 20;

/// 当场探歌词时同时问几首。
const PROBES_AT_ONCE: usize = 4;

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
    /// 筛选条件,`[FacetPickDto]` 的 JSON。没有就是不筛。
    pub(crate) filter: Option<String>,
}

/// 往平台要一批的办法。心动模式的歌单 id 只问一次,不随每回拉取重问。
enum Source {
    /// 私人 FM 不挂在歌单上;红心歌单 id 只拿来排除红心过的歌,问不到就只认本地「我的喜欢」。
    Fm {
        liked_playlist: Option<String>,
    },
    Heart {
        seed: String,
        playlist_id: String,
    },
}

/// `GET /radio?mode=fm` / `GET /radio?mode=heart&seed=<曲目 id>` —— 一批没听过的新歌。
/// 再带 `&filter=<JSON>` 就只要过得了筛选的。
pub(crate) async fn radio(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<RadioQuery>,
) -> Result<Json<TracksDto>, Failure> {
    let picks: Vec<FacetPickDto> = match &query.filter {
        None => Vec::new(),
        Some(raw) => serde_json::from_str(raw).map_err(|err| {
            fail(&tonic::Status::invalid_argument(format!(
                "筛选条件读不懂: {err}"
            )))
        })?,
    };
    let max_pulls = if picks.is_empty() {
        MAX_PULLS
    } else {
        MAX_FILTERED_PULLS
    };
    let source = match query.mode {
        RadioMode::Fm => Source::Fm {
            // 问不到(网易云没登录、上游一时失败)不挡电台:私人 FM 自己也会报那个错
            liked_playlist: netease_liked_id(
                &state, &account,
            )
            .await
            .ok(),
        },
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
    let mut probed: Vec<(String, LyricKind)> = Vec::new();
    for pull in 1..=max_pulls {
        let batch = source.pull(&state, &account).await?;
        let fetched = batch.len();
        let fresh = keep_fresh(
            &state,
            account.id,
            batch,
            &picked,
            source.liked_playlist(),
        )
        .await?;
        let unheard = fresh.len();
        let mut fresh = fresh;
        let asked = probed.len();
        if filter::wants_lyric(&picks) {
            probe_lyrics(
                &state,
                &account,
                &mut fresh,
                &picks,
                &mut probed,
            )
            .await;
        }
        let kept: Vec<TrackDto> = fresh
            .into_iter()
            .filter(|track| filter::passes(track, &picks, None))
            .collect();
        tracing::info!(
            mode = ?query.mode,
            pull,
            fetched,
            dropped = fetched - unheard,
            filtered = unheard - kept.len(),
            probed = probed.len() - asked,
            "电台拉了一批"
        );
        picked.extend(kept);
        if picked.len() >= ENOUGH || fetched == 0 {
            break;
        }
    }
    remember_details(&state, &picked).await;
    remember_lyrics(&state, &probed).await;

    // 聚合在每批进来时已经填过(电台区也要分组筛选,#160)。藏掉几首不报:
    // 电台本来就是挑剩下的,少的那几首换一批就补上了
    Ok(Json(TracksDto {
        tracks: picked,
        unavailable: 0,
        hidden: 0,
    }))
}

/// 筛选要看歌词时,`tracks` 里还没探过、其余维度已经过了的当场探一次,
/// 探到的标记写回曲目并记进 `probed`。一次请求一共最多探 [`MAX_PROBES`] 首;
/// 探不到的留着 `None`,由筛选当「歌词未知」处理。
async fn probe_lyrics(
    state: &AppState,
    account: &Account,
    tracks: &mut [TrackDto],
    picks: &[FacetPickDto],
    probed: &mut Vec<(String, LyricKind)>,
) {
    let budget = MAX_PROBES.saturating_sub(probed.len());
    let wanted: Vec<(usize, String)> = tracks
        .iter()
        .enumerate()
        .filter(|(_, track)| {
            track.facets.lyric_kind.is_none()
                && filter::passes(
                    track,
                    picks,
                    Some(FacetDto::Lyric),
                )
        })
        .map(|(at, track)| (at, track.id.clone()))
        .take(budget)
        .collect();
    let answers: Vec<(usize, String, Result<LyricKind, String>)> =
        futures_util::stream::iter(wanted)
            .map(|(at, id)| async move {
                let kind =
                    lyric_probe::ask(state, account, &id).await;
                (at, id, kind)
            })
            .buffer_unordered(PROBES_AT_ONCE)
            .collect()
            .await;
    for (at, id, kind) in answers {
        match kind {
            Ok(kind) => {
                tracks[at].facets.lyric_kind =
                    Some(kind.to_dto());
                probed.push((id, kind));
            }
            Err(err) => {
                tracing::warn!(track_id = %id, %err, "电台当场探歌词失败");
            }
        }
    }
}

/// 当场探到的歌词标记落库。要在详情进了缓存之后:还没进缓存的歌没有行可写。
/// 记不上只写日志 —— 后台队列迟早会再探一遍。
async fn remember_lyrics(
    state: &AppState,
    probed: &[(String, LyricKind)],
) {
    if probed.is_empty() {
        return;
    }
    let mut conn = match state.pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(%err, "当场探的歌词标记记不上");
            return;
        }
    };
    for (id, kind) in probed {
        if let Err(err) =
            lyric::record(&mut conn, NETEASE, id, *kind).await
        {
            tracing::warn!(track_id = %id, ?err, "当场探的歌词标记记不上");
        }
    }
}

/// 电台只有网易云。
const NETEASE: &str = "netease";

impl Source {
    /// 平台红心歌单的 id,排除红心过的歌时认它在缓存里的那份。
    fn liked_playlist(&self) -> Option<&str> {
        match self {
            Self::Fm { liked_playlist } => {
                liked_playlist.as_deref()
            }
            Self::Heart { playlist_id, .. } => {
                Some(playlist_id)
            }
        }
    }

    async fn pull(
        &self,
        state: &AppState,
        account: &Account,
    ) -> Result<Vec<TrackDto>, Failure> {
        let mut discover = state.upstream.discover.clone();
        let tracks = match self {
            Self::Fm { .. } => {
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

/// 电台的过滤,一处收口(#161;#166 的按筛选续歌叠在它上面):
/// 先过出口那一道(填聚合、滤掉命中屏蔽规则的),再丢掉听过、红心过、赞踩过的,
/// 以及已经挑进 `picked` 的。`liked_playlist` 是平台红心歌单 id,见 [`history::known_among`]。
pub(crate) async fn keep_fresh(
    state: &AppState,
    account_id: i64,
    batch: Vec<TrackDto>,
    picked: &[TrackDto],
    liked_playlist: Option<&str>,
) -> Result<Vec<TrackDto>, Failure> {
    let shaped = for_account(
        state,
        account_id,
        TracksDto {
            tracks: batch,
            unavailable: 0,
            hidden: 0,
        },
    )
    .await
    .tracks;
    unheard(
        state,
        account_id,
        shaped,
        picked,
        liked_playlist,
    )
    .await
}

/// `batch` 里没听过、没表过态、也不在 `picked` 里的那些。同一批里重复的只留第一首。
async fn unheard(
    state: &AppState,
    account_id: i64,
    batch: Vec<TrackDto>,
    picked: &[TrackDto],
    liked_playlist: Option<&str>,
) -> Result<Vec<TrackDto>, Failure> {
    let refs: Vec<TrackRef> = batch
        .iter()
        .map(|track| TrackRef {
            platform: track.platform.clone(),
            track_id: track.id.clone(),
        })
        .collect();
    let mut conn = conn(&state.pool).await?;
    let played = history::known_among(
        &mut conn,
        account_id,
        &refs,
        liked_playlist,
    )
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

mod filter;

#[cfg(test)]
mod tests;
