//! 预取:把该存的曲目以无损存进桶,不等谁去播它(#147)。
//!
//! 队列在 Postgres(`server::store::prefetch`),这里是入队的入口与后台 worker。
//! 入队的时机:每日推荐取回来之后、我们的歌单加了曲目(含红心与导入)、
//! `/played` 报上一首还没存的,以及进程起来时把所有歌单排一遍。
//!
//! 每个音源一组 worker、一个限速器:worker 数管同时下几首(每首整个读进内存、
//! 与正在播放的那首抢出口带宽),限速器管多久问一次音源。

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::{
    DefaultDirectRateLimiter, Quota, RateLimiter,
};

use server::store::account;
use server::store::playlist::TrackRef;
use server::store::prefetch::{self, Job, Unfinished};

use crate::AppState;
use crate::routes::play::archive::{self, NETEASE, Stored};

#[cfg(test)]
mod tests;

/// 领走一个任务后多久没办完算领的人死了,别人可以再领。一首无损几十 MB,
/// 十分钟足够下完;过了还没完多半是进程没了。
const LEASE: Duration = Duration::from_secs(600);

/// 失败后的退避单位:第 n 次失败等 n 个它。
const BACKOFF: Duration = Duration::from_secs(600);

/// 领过几次还失败就记 failed,等下一次入队再试。
const MAX_ATTEMPTS: i32 = 5;

/// 没任务时隔多久再看一眼。入队不叫醒 worker,最多晚这么久开工。
const IDLE: Duration = Duration::from_secs(30);

/// worker 的上限:每个音源几个 worker、每分钟最多问几次。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) workers: usize,
    pub(crate) per_minute: NonZeroU32,
}

impl Limits {
    /// 两首同时存足够跟上,再多只是和播放抢带宽;每分钟二十首,
    /// 981 首的「我的喜欢」一小时左右排完,不至于把网易云问烦。
    pub(crate) const DEFAULT: Self = Self {
        workers: 2,
        per_minute: NonZeroU32::new(20).expect("非零"),
    };

    /// 由环境变量 `PREFETCH_WORKERS`、`PREFETCH_NETEASE_PER_MINUTE` 覆盖默认值。
    /// 写成零或不是数的,用默认值并记一行。
    pub(crate) fn from_env() -> Self {
        let workers = env_number("PREFETCH_WORKERS")
            .and_then(|n| usize::try_from(n.get()).ok())
            .unwrap_or(Self::DEFAULT.workers);
        let per_minute =
            env_number("PREFETCH_NETEASE_PER_MINUTE")
                .unwrap_or(Self::DEFAULT.per_minute);
        Self {
            workers,
            per_minute,
        }
    }
}

fn env_number(name: &str) -> Option<NonZeroU32> {
    let raw = std::env::var(name).ok()?;
    let parsed = raw.trim().parse().ok();
    if parsed.is_none() {
        tracing::warn!(name, raw, "不是正整数,用默认值");
    }
    parsed
}

/// 以这个账号的凭据把这些曲目排进队列。没配对象存储就什么都不做。
///
/// 失败只记日志:入队是顺手的事,不该让加歌、点心、取日推失败。
pub(crate) async fn enqueue(
    state: &AppState,
    account_id: i64,
    tracks: &[TrackRef],
) {
    if state.archive.is_none() || tracks.is_empty() {
        return;
    }
    let queued = match state.pool.acquire().await {
        Ok(mut conn) => prefetch::enqueue(
            &mut conn,
            account_id,
            tracks,
            &archive::quality(),
        )
        .await
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    match queued {
        Ok(0) => {}
        Ok(queued) => {
            tracing::info!(queued, "排进预取队列")
        }
        Err(err) => {
            tracing::warn!(%err, "排不进预取队列")
        }
    }
}

/// 起 worker,并把所有歌单排一遍(首次上线时就是全部存量)。没配对象存储就不起。
pub(crate) fn spawn(state: &AppState, limits: Limits) {
    if state.archive.is_none() {
        return;
    }
    let seed = state.clone();
    tokio::spawn(async move {
        let queued = match seed.pool.acquire().await {
            Ok(mut conn) => {
                prefetch::enqueue_all_playlists(
                    &mut conn,
                    &archive::quality(),
                )
                .await
                .map_err(|err| format!("{err:?}"))
            }
            Err(err) => Err(err.to_string()),
        };
        match queued {
            Ok(queued) => tracing::info!(
                queued,
                "歌单里的曲目排进预取队列"
            ),
            Err(err) => {
                tracing::warn!(%err, "歌单排不进预取队列")
            }
        }
    });

    tracing::info!(
        workers = limits.workers,
        per_minute = limits.per_minute.get(),
        "预取 worker 起来了"
    );
    let limiter: Arc<DefaultDirectRateLimiter> =
        Arc::new(RateLimiter::direct(Quota::per_minute(
            limits.per_minute,
        )));
    for _ in 0..limits.workers {
        let state = state.clone();
        let limiter = limiter.clone();
        tokio::spawn(async move {
            loop {
                if !step(&state, NETEASE, &limiter).await {
                    tokio::time::sleep(IDLE).await;
                }
            }
        });
    }
}

/// 领一个任务办掉。没领到(或领的时候出错)返回 `false`,调用方歇一会儿。
async fn step(
    state: &AppState,
    platform: &str,
    limiter: &DefaultDirectRateLimiter,
) -> bool {
    let claimed = match state.pool.acquire().await {
        Ok(mut conn) => {
            prefetch::claim(&mut conn, platform, LEASE)
                .await
                .map_err(|err| format!("{err:?}"))
        }
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
            tracing::warn!(%err, "领不了预取任务");
            false
        }
    }
}

/// 办一个已经领到的任务:存,再按结果删行、记原因或退避。
pub(crate) async fn run(state: &AppState, job: &Job) {
    let track = job.track();
    let outcome = match state.pool.acquire().await {
        Ok(mut conn) => {
            account::find(&mut conn, job.account_id)
                .await
                .map_err(|err| format!("{err:?}"))
        }
        Err(err) => Err(err.to_string()),
    };
    let outcome = match outcome {
        // 账号删了,任务本该随它级联删掉;走到这里只是恰好撞上,当办完
        Ok(None) => Ok(Stored::Already),
        Ok(Some(account)) => {
            archive::store_track(state, &account, &track)
                .await
        }
        Err(err) => Err(err),
    };

    let settled = match state.pool.acquire().await {
        Ok(mut conn) => match &outcome {
            Ok(Stored::Now | Stored::Already) => {
                prefetch::finish(&mut conn, job).await
            }
            Ok(Stored::NoLossless) => {
                prefetch::settle(
                    &mut conn,
                    job,
                    Unfinished::NoLossless,
                )
                .await
            }
            Err(err) => {
                tracing::warn!(
                    track_id = %job.track_id,
                    attempts = job.attempts,
                    %err,
                    "预取失败,退避后再试"
                );
                prefetch::retry_later(
                    &mut conn,
                    job,
                    BACKOFF,
                    MAX_ATTEMPTS,
                )
                .await
            }
        }
        .map_err(|err| format!("{err:?}")),
        Err(err) => Err(err.to_string()),
    };
    // 记不上的话租约到期会再领一次,最多重办一遍
    if let Err(err) = settled {
        tracing::warn!(track_id = %job.track_id, %err, "预取任务的结果记不上");
    }
}
