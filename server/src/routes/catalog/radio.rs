//! 电台(#159):私人 FM 与心动模式,一条路由两种模式,都为了推新歌。
//!
//! 听过的一律丢掉(用户 2026-09-27 定):判据是这个账号的 `play_events`;
//! 红心过、赞踩过、命中屏蔽规则的同样丢掉(2026-09-28 并进 #161)。
//! 丢完不够就再问平台,但一次请求最多问 [`MAX_PULLS`] 回 —— 过滤太狠时
//! 会连着问,而连着问平台就是风控的靶子。问满了仍不够,交出已有的。
//!
//! 带着筛选来的(#166,电台区选了 chip 再点歌)在排除之后再按筛选过一道;
//! 筛选越窄丢得越多,上限放到 [`MAX_FILTERED_PULLS`]。
//!
//! 共享歌单一次补 [`REFILL`] 首:私人 FM 一回只给 3 首,听得久的账号几乎全被上面的
//! 过滤丢光,只靠它歌单会被抽干(线上见过未听 0 首、已听 401 首)。FM 问满还不够,
//! 就以最近听过的歌为种子接心动模式,再不够接当天日推;一个来源失败不挡下一个。
//!
//! 新歌多半还没探过歌词,每批都当场探一次(见 [`probe_lyrics`]),不等后台队列:
//! 不带筛选的起播那一批也探,电台区一摆出来就有「有歌词」可选。

use axum::{
    Json,
    extract::{Query, State},
};
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use contract::{
    FacetDto, FacetPickDto, RadioListDto, ServerSignal,
    TrackDto, TracksDto,
};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use server::bangdream::{
    self,
    proto::{
        GetDailyRecommendationsRequest,
        GetIntelligenceListRequest, GetPersonalFmRequest,
        Platform,
    },
};
use server::error::{self, Failure};
use server::store::account::{self, Account};
use server::store::cache;
use server::store::history;
use server::store::lyric::{self, LyricKind};
use server::store::playlist::TrackRef;
use server::store::radio;
use server::syncplay::signaling;

use super::lyric_probe;
use super::search::{refresh_daily, remember_details};
use crate::routes::library::for_account;
use crate::routes::library::likes::netease_liked_id;
use crate::routes::play::prefetch;
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

/// 给共享歌单补歌时攒到多少首为止。一首歌三四分钟,够放一个多小时再来问平台。
pub(crate) const REFILL: usize = 20;

/// 电台与日推合起来未听的少于这么多首,服务端自己补一批。用户定的 500。
/// 预取三分钟存一首,新歌要排在够远的后面,放到它时才已经存好。
pub(crate) const TOP_UP_BELOW: usize = 500;

/// 后台多久看一眼存量:补电台,取当天日推。
const STOCK_EVERY: Duration = Duration::from_secs(1800);

/// 正在后台补歌的账号。切歌快时 `/played` 接连到,一个账号同时只补一趟。
static TOPPING_UP: LazyLock<Mutex<HashSet<i64>>> =
    LazyLock::new(Mutex::default);

/// 补歌时 FM 不够,以几首听过的歌为种子接心动模式。心动一回给几十首,一两回足够。
const REFILL_SEEDS: usize = 2;

/// 补歌最多花多久。客户端读超时 10 秒,而平台问一回就要一两秒:到点不再开新的一问,
/// 手上有多少交多少,留出时间给还在路上的那一问。
const REFILL_BUDGET: Duration = Duration::from_secs(6);

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
    /// 当天日推:补歌的最后一道,不要种子。
    Daily {
        liked_playlist: Option<String>,
    },
}

/// 这一趟要攒多少。
#[derive(Clone, Copy)]
struct Goal {
    /// 最多问平台几回。
    pulls: usize,
    /// 攒到这么多首就不再问。
    enough: usize,
    /// 不带筛选时最多留几首:心动一回几十首,多的不探歌词也不记。
    cap: usize,
    /// 过了这一刻不再问平台,歌词也不当场探。
    deadline: Option<Instant>,
}

impl Goal {
    fn late(&self) -> bool {
        self.deadline.is_some_and(|at| Instant::now() >= at)
    }
}

/// `GET /radio?mode=fm` / `GET /radio?mode=heart&seed=<曲目 id>` —— 一批没听过的新歌。
/// 再带 `&filter=<JSON>` 就只要过得了筛选的。
pub(crate) async fn radio(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<RadioQuery>,
) -> Result<Json<TracksDto>, Failure> {
    let picks = parse_picks(query.filter.as_deref())?;
    let goal = Goal {
        pulls: pulls_for(&picks),
        enough: ENOUGH,
        cap: usize::MAX,
        deadline: None,
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

    let picked = gather(
        &state,
        &account,
        &source,
        &picks,
        goal,
        &[],
    )
    .await?;

    // 聚合在每批进来时已经填过(电台区也要分组筛选,#160)。藏掉几首不报:
    // 电台本来就是挑剩下的,少的那几首换一批就补上了
    Ok(Json(TracksDto {
        tracks: picked,
        unavailable: 0,
        hidden: 0,
    }))
}

/// 筛选条件,`[FacetPickDto]` 的 JSON。没有就是不筛。
fn parse_picks(
    raw: Option<&str>,
) -> Result<Vec<FacetPickDto>, Failure> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    serde_json::from_str(raw).map_err(|err| {
        fail(&tonic::Status::invalid_argument(format!(
            "筛选条件读不懂: {err}"
        )))
    })
}

/// 带筛选丢得多,问平台的上限放宽。
fn pulls_for(picks: &[FacetPickDto]) -> usize {
    if picks.is_empty() {
        MAX_PULLS
    } else {
        MAX_FILTERED_PULLS
    }
}

#[derive(Deserialize)]
pub(crate) struct MoreQuery {
    /// 同 [`RadioQuery::filter`]。
    pub(crate) filter: Option<String>,
}

/// `GET /radio/list` —— 这个账号那一份共享电台歌单(#186)。
pub(crate) async fn list(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<RadioListDto>, Failure> {
    Ok(Json(shared_list(&state, account.id).await?))
}

/// `POST /radio/more` —— 补一批新歌进共享歌单,交回追加之后的整份(来源见 [`refill`])。
/// 再带 `?filter=<JSON>` 就只加过得了筛选的(电台区选着 chip 续歌,#166)。
///
/// 加进来的立刻排进预取(存进 RustFS),并告诉账号下每台在线设备去重取。
/// 一首都没加上就不广播:歌单没变。
pub(crate) async fn more(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<MoreQuery>,
) -> Result<Json<RadioListDto>, Failure> {
    let picks = parse_picks(query.filter.as_deref())?;
    let before = shared_list(&state, account.id).await?;
    Ok(Json(
        load_more(&state, &account, &picks, before).await?,
    ))
}

/// 每隔 [`STOCK_EVERY`] 替用过电台或日推的账号备货,不等谁打开客户端。
pub(crate) fn spawn_stocker(
    state: &AppState,
    stop: &CancellationToken,
) {
    let state = state.clone();
    let stop = stop.clone();
    tokio::spawn(async move {
        let mut every = tokio::time::interval(STOCK_EVERY);
        while stop
            .run_until_cancelled(every.tick())
            .await
            .is_some()
        {
            if let Err(err) = stock(&state).await {
                tracing::warn!(%err, "后台备货没做成");
            }
        }
    });
}

async fn stock(state: &AppState) -> Result<(), String> {
    let mut conn = state
        .pool
        .acquire()
        .await
        .map_err(|err| err.to_string())?;
    let accounts = radio::listeners(&mut conn)
        .await
        .map_err(|err| format!("{err:?}"))?;
    for id in accounts {
        let Some(found) = account::find(&mut conn, id)
            .await
            .map_err(|err| format!("{err:?}"))?
        else {
            continue;
        };
        if refresh_daily(state, &found).await.is_err() {
            tracing::warn!(account = id, "后台取日推没成");
        }
        spawn_top_up(state, found);
    }
    Ok(())
}

/// 听着电台时后台看一眼存量,不够就补。补不到只记日志:客户端见底时还会自己来要。
pub(crate) fn spawn_top_up(
    state: &AppState,
    account: Account,
) {
    if !TOPPING_UP.lock().unwrap().insert(account.id) {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        if top_up(&state, &account, TOP_UP_BELOW)
            .await
            .is_err()
        {
            tracing::warn!("电台后台补歌没成");
        }
        TOPPING_UP.lock().unwrap().remove(&account.id);
    });
}

/// 电台与日推合起来未听的少于 `below` 首就补一批。
async fn top_up(
    state: &AppState,
    account: &Account,
    below: usize,
) -> Result<(), Failure> {
    let unheard = {
        let mut conn = conn(&state.pool).await?;
        radio::unheard_total(&mut conn, account.id)
            .await
            .map_err(|err| error::map_error(&err))?
    };
    if unheard < below {
        let before = shared_list(state, account.id).await?;
        load_more(state, account, &[], before).await?;
    }
    Ok(())
}

/// 补一批进共享歌单,排进预取,通知各台,交回追加之后的整份。
async fn load_more(
    state: &AppState,
    account: &Account,
    picks: &[FacetPickDto],
    before: RadioListDto,
) -> Result<RadioListDto, Failure> {
    let fresh =
        refill(state, account, picks, &before).await?;
    if fresh.is_empty() {
        return Ok(before);
    }
    let refs: Vec<TrackRef> = fresh
        .iter()
        .map(|track| TrackRef {
            platform: track.platform.clone(),
            track_id: track.id.clone(),
        })
        .collect();
    let added = {
        let mut conn = conn(&state.pool).await?;
        radio::append(&mut conn, account.id, &refs)
            .await
            .map_err(|err| error::map_error(&err))?
    };
    tracing::info!(added, "电台歌单追加了新歌");
    prefetch::enqueue(state, account.id, &refs).await;
    tell_radio_changed(state, account.id);
    shared_list(state, account.id).await
}

/// 补歌的来源,先问的在前:私人 FM,以最近听过的歌为种子的心动模式,当天日推。
/// 红心歌单 id 问不到时心动没法问,跳过它。
fn refill_sources(
    liked_playlist: Option<String>,
    heard: &[TrackDto],
) -> Vec<Source> {
    let mut sources = vec![Source::Fm {
        liked_playlist: liked_playlist.clone(),
    }];
    if let Some(playlist_id) = &liked_playlist {
        sources.extend(
            heard.iter().rev().take(REFILL_SEEDS).map(
                |track| Source::Heart {
                    seed: track.id.clone(),
                    playlist_id: playlist_id.clone(),
                },
            ),
        );
    }
    sources.push(Source::Daily { liked_playlist });
    sources
}

/// 依次问各来源,攒够 [`REFILL`] 首为止。一个来源失败只记日志,全都一首没给才报错。
async fn refill(
    state: &AppState,
    account: &Account,
    picks: &[FacetPickDto],
    before: &RadioListDto,
) -> Result<Vec<TrackDto>, Failure> {
    let liked_playlist =
        netease_liked_id(state, account).await.ok();
    let deadline = Instant::now() + REFILL_BUDGET;
    let mut known = before.tracks.clone();
    let mut failed = None;
    for source in
        refill_sources(liked_playlist, &before.heard)
    {
        let room = REFILL.saturating_sub(
            known.len() - before.tracks.len(),
        );
        let goal = Goal {
            pulls: source.pulls(picks),
            enough: room,
            cap: room,
            deadline: Some(deadline),
        };
        // 到点了但一首都没有时还是往下问:空手回去,电台就真没歌放了
        let got = known.len() > before.tracks.len();
        if room == 0 || (goal.late() && got) {
            break;
        }
        match gather(
            state, account, &source, picks, goal, &known,
        )
        .await
        {
            Ok(found) => known.extend(found),
            Err(failure) => {
                tracing::warn!(
                    source = source.name(),
                    "电台补歌这一路没问到"
                );
                failed.get_or_insert(failure);
            }
        }
    }
    let fresh = known.split_off(before.tracks.len());
    match failed {
        Some(failure) if fresh.is_empty() => Err(failure),
        _ => Ok(fresh),
    }
}

/// 告诉账号下每台在线设备:电台歌单变了,去重取。
pub(crate) fn tell_radio_changed(
    state: &AppState,
    account_id: i64,
) {
    signaling::tell_account(
        &state.roster,
        account_id,
        &ServerSignal::RadioChanged,
    );
}

/// 读出共享歌单,按听没听过分成两半,各自过一遍出口(填聚合、滤屏蔽规则)。
/// 缓存里没有详情的跳过 —— 加进来时详情已经写过,缺的只会是平台后来不给了的。
async fn shared_list(
    state: &AppState,
    account_id: i64,
) -> Result<RadioListDto, Failure> {
    let entries = {
        let mut conn = conn(&state.pool).await?;
        radio::list(&mut conn, account_id)
            .await
            .map_err(|err| error::map_error(&err))?
    };
    split_heard(state, account_id, entries).await
}

/// 把歌单条目分成没听的和听过的,配上详情,各自过一遍出口。电台与日推共用。
pub(crate) async fn split_heard(
    state: &AppState,
    account_id: i64,
    entries: Vec<radio::Entry>,
) -> Result<RadioListDto, Failure> {
    let mut conn = conn(&state.pool).await?;
    let ids: Vec<String> = entries
        .iter()
        .filter(|entry| entry.track.platform == NETEASE)
        .map(|entry| entry.track.track_id.clone())
        .collect();
    let details =
        cache::details_of(&mut conn, NETEASE, &ids)
            .await
            .map_err(|err| error::map_error(&err))?;
    drop(conn);
    let heard: HashSet<&str> = entries
        .iter()
        .filter(|entry| entry.heard)
        .map(|entry| entry.track.track_id.as_str())
        .collect();
    let (heard, unheard): (Vec<TrackDto>, Vec<TrackDto>) =
        details.into_iter().partition(|track| {
            heard.contains(track.id.as_str())
        });
    let shape = |tracks| {
        for_account(
            state,
            account_id,
            TracksDto {
                tracks,
                unavailable: 0,
                hidden: 0,
            },
        )
    };
    // 日推的推荐日挂到每一首上(列表分「今天」「昨天」),出口填完聚合之后再挂
    let days: std::collections::HashMap<&str, &str> =
        entries
            .iter()
            .filter_map(|entry| {
                Some((
                    entry.track.track_id.as_str(),
                    entry.picked_on.as_deref()?,
                ))
            })
            .collect();
    let dated = |mut tracks: Vec<TrackDto>| {
        for track in &mut tracks {
            track.facets.picked_on = days
                .get(track.id.as_str())
                .map(|day| (*day).to_owned());
        }
        tracks
    };
    Ok(RadioListDto {
        tracks: dated(shape(unheard).await.tracks),
        heard: dated(shape(heard).await.tracks),
    })
}

/// 向平台拉新歌,丢掉听过、表过态的以及 `already` 里已有的,过一遍筛选,
/// 攒够 `goal` 或问满回数为止。交出的详情与当场探到的歌词记进库。
/// 问到一半失败时已攒到的照样交出去,一首都没有才报错。
async fn gather(
    state: &AppState,
    account: &Account,
    source: &Source,
    picks: &[FacetPickDto],
    goal: Goal,
    already: &[TrackDto],
) -> Result<Vec<TrackDto>, Failure> {
    let mut picked: Vec<TrackDto> = already.to_vec();
    let mut probed: Vec<(String, LyricKind)> = Vec::new();
    for pull in 1..=goal.pulls {
        if pull > 1 && goal.late() {
            break;
        }
        let batch = match source.pull(state, account).await
        {
            Ok(batch) => batch,
            Err(failure)
                if picked.len() == already.len() =>
            {
                return Err(failure);
            }
            Err(_) => {
                tracing::warn!(
                    pull,
                    "电台拉到一半失败,交出已攒到的"
                );
                break;
            }
        };
        let fetched = batch.len();
        let fresh = keep_fresh(
            state,
            account.id,
            batch,
            &picked,
            source.liked_playlist(),
        )
        .await?;
        let unheard = fresh.len();
        let mut fresh = fresh;
        if picks.is_empty() {
            fresh.truncate(goal.cap.saturating_sub(
                picked.len() - already.len(),
            ));
        }
        let considered = fresh.len();
        let asked = probed.len();
        if !goal.late() {
            probe_lyrics(
                state,
                account,
                &mut fresh,
                picks,
                &mut probed,
            )
            .await;
        }
        let kept: Vec<TrackDto> = fresh
            .into_iter()
            .filter(|track| {
                filter::passes(track, picks, None)
            })
            .collect();
        tracing::info!(
            pull,
            fetched,
            dropped = fetched - unheard,
            filtered = considered - kept.len(),
            probed = probed.len() - asked,
            "电台拉了一批"
        );
        picked.extend(kept);
        if picked.len() - already.len() >= goal.enough
            || fetched == 0
        {
            break;
        }
    }
    let picked = picked.split_off(already.len());
    remember_details(state, &picked).await;
    remember_lyrics(state, &probed).await;
    Ok(picked)
}

/// `tracks` 里还没探过歌词、其余维度已经过了筛选的当场探一次,
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
    let answers: Vec<(
        usize,
        String,
        Result<LyricKind, String>,
    )> = futures_util::stream::iter(wanted)
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
            lyric::record(&mut conn, NETEASE, id, *kind)
                .await
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
            Self::Fm { liked_playlist }
            | Self::Daily { liked_playlist } => {
                liked_playlist.as_deref()
            }
            Self::Heart { playlist_id, .. } => {
                Some(playlist_id)
            }
        }
    }

    /// 补歌时这一路最多问几回。只有 FM 一回给得少,要多问。
    fn pulls(&self, picks: &[FacetPickDto]) -> usize {
        match self {
            Self::Fm { .. } => pulls_for(picks),
            Self::Heart { .. } | Self::Daily { .. } => 1,
        }
    }

    /// 日志里的名字。
    fn name(&self) -> &'static str {
        match self {
            Self::Fm { .. } => "fm",
            Self::Heart { .. } => "heart",
            Self::Daily { .. } => "daily",
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
            Self::Daily { .. } => discover
                .get_daily_recommendations(
                    bangdream::as_user(
                        account,
                        GetDailyRecommendationsRequest {
                            platform: Platform::Netease
                                as i32,
                        },
                    ),
                )
                .await
                .map_err(|status| fail(&status))?
                .into_inner()
                .tracks,
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
mod shared_tests;
#[cfg(test)]
mod tests;
