//! 歌词探测:给 `platform_tracks` 里每一首还没探过的歌问一次 `GetLyric`,
//! 记下有没有歌词(#156)。歌单分类视图要按它分组,而没听过的歌也得有标记,
//! 所以不等播放时顺带回填。
//!
//! 队列在 `server::store::lyric`,这里是后台 worker:一个,限速,一次一首 ——
//! 每首只占一份歌词的内存,生产 512Mi 下不必再分批。

use std::num::NonZeroU32;
use std::time::Duration;

use governor::{
    DefaultDirectRateLimiter, Quota, RateLimiter,
};
use tokio_util::sync::CancellationToken;
use tonic::Code;

use server::bangdream::{
    self,
    proto::{self, GetLyricRequest, Platform},
};
use server::store::account;
use server::store::lyric::{self, Job, LyricKind};

use crate::AppState;

#[cfg(test)]
mod tests;

/// 探测的只有网易云:目前唯一的平台。
const NETEASE: &str = "netease";

/// 领走后多久没办完算领的人死了。一次歌词请求几秒就回。
const LEASE: Duration = Duration::from_secs(120);

/// 失败后的退避单位:第 n 次失败等 n 个它。
const BACKOFF: Duration = Duration::from_secs(600);

/// 领过几次还失败就不再领,留在 unknown。
const MAX_ATTEMPTS: i32 = 5;

/// 没活时隔多久再看一眼。写进缓存不叫醒 worker,最多晚这么久开工。
const IDLE: Duration = Duration::from_secs(30);

/// 默认每分钟问几次。估的 1 次/秒,按网易云风控实测再调:
/// 环境变量 `LYRIC_PROBE_PER_MINUTE` 覆盖。
const DEFAULT_PER_MINUTE: NonZeroU32 =
    NonZeroU32::new(60).expect("非零");

/// 纯音乐的歌词里平台写的那句话。网易云的原文是「纯音乐,请欣赏」。
const INSTRUMENTAL_MARK: &str = "纯音乐";

/// 纯音乐的「歌词」只有这句话,前面最多挂几行作曲、编曲之类的署名。
/// 行数再多就是真歌词里恰好提到了纯音乐。
const INSTRUMENTAL_MAX_LINES: usize = 5;

/// 一份歌词算哪一类。
pub(crate) fn kind_of(lyric: &proto::Lyric) -> LyricKind {
    let lines = &lyric.lines;
    if lines.is_empty() {
        return LyricKind::None;
    }
    if lines.len() <= INSTRUMENTAL_MAX_LINES
        && lines.iter().any(|line| {
            line.text.contains(INSTRUMENTAL_MARK)
        })
    {
        return LyricKind::Instrumental;
    }
    if lines.iter().any(|line| !line.translation.is_empty())
    {
        return LyricKind::Translated;
    }
    LyricKind::Lyric
}

/// 每分钟问几次:`LYRIC_PROBE_PER_MINUTE`,没设或不是正整数就用默认值。
fn per_minute() -> NonZeroU32 {
    let Ok(raw) = std::env::var("LYRIC_PROBE_PER_MINUTE")
    else {
        return DEFAULT_PER_MINUTE;
    };
    raw.trim().parse().unwrap_or_else(|_| {
        tracing::warn!(
            raw,
            "LYRIC_PROBE_PER_MINUTE 不是正整数,用默认值"
        );
        DEFAULT_PER_MINUTE
    })
}

/// 起 worker。`stop` 一到就不再领。
pub(crate) fn spawn(
    state: &AppState,
    stop: &CancellationToken,
) {
    let per_minute = per_minute();
    tracing::info!(
        per_minute = per_minute.get(),
        "歌词探测 worker 起来了"
    );
    let limiter =
        RateLimiter::direct(Quota::per_minute(per_minute));
    let state = state.clone();
    let stop = stop.clone();
    tokio::spawn(async move {
        while let Some(busy) = stop
            .run_until_cancelled(step(
                &state, NETEASE, &limiter,
            ))
            .await
        {
            if !busy
                && stop
                    .run_until_cancelled(
                        tokio::time::sleep(IDLE),
                    )
                    .await
                    .is_none()
            {
                return;
            }
        }
    });
}

/// 领一首探掉。没领到(或领的时候出错)返回 `false`,调用方歇一会儿。
pub(crate) async fn step(
    state: &AppState,
    platform: &str,
    limiter: &DefaultDirectRateLimiter,
) -> bool {
    let claimed = match state.pool.acquire().await {
        Ok(mut conn) => lyric::claim(
            &mut conn,
            platform,
            LEASE,
            MAX_ATTEMPTS,
        )
        .await
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    match claimed {
        Ok(Some(job)) => {
            limiter.until_ready().await;
            run(state, &job).await;
            true
        }
        Ok(None) => false,
        Err(err) => {
            tracing::warn!(%err, "领不了歌词探测任务");
            false
        }
    }
}

/// 问平台要这首的歌词,翻成标记。平台说这首不存在(下架)也当没有歌词。
async fn probe(
    state: &AppState,
    job: &Job,
) -> Result<LyricKind, String> {
    let account = match state.pool.acquire().await {
        Ok(mut conn) => {
            account::find(&mut conn, job.account_id)
                .await
                .map_err(|err| format!("{err:?}"))?
        }
        Err(err) => return Err(err.to_string()),
    }
    .ok_or("问歌词的账号刚被删了")?;

    let mut catalog = state.upstream.catalog.clone();
    let asked = catalog
        .get_lyric(bangdream::as_user(
            &account,
            GetLyricRequest {
                platform: Platform::Netease as i32,
                track_id: job.track_id.clone(),
            },
        ))
        .await;
    match asked {
        Ok(response) => Ok(kind_of(
            &response
                .into_inner()
                .lyric
                .unwrap_or_default(),
        )),
        Err(status) if status.code() == Code::NotFound => {
            Ok(LyricKind::None)
        }
        Err(status) => Err(status.to_string()),
    }
}

/// 办一个已经领到的任务:探,再按结果记标记或退避。
async fn run(state: &AppState, job: &Job) {
    let outcome = probe(state, job).await;
    let settled = match state.pool.acquire().await {
        Ok(mut conn) => match outcome {
            Ok(kind) => {
                lyric::settle(&mut conn, job, kind).await
            }
            Err(err) => {
                tracing::warn!(
                    track_id = %job.track_id,
                    attempts = job.attempts,
                    %err,
                    "歌词探测失败,退避后再试"
                );
                lyric::retry_later(&mut conn, job, BACKOFF)
                    .await
            }
        }
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    // 记不上的话租约到期会再领一次,最多重探一遍
    if let Err(err) = settled {
        tracing::warn!(track_id = %job.track_id, %err, "歌词探测的结果记不上");
    }
}
