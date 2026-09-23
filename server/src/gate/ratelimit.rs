//! 限流:同一个键在一段时间里最多放行几次。
//!
//! 用 `tower_governor` 的 GCRA,不再自己写固定窗口 —— 从前那份手写的只有
//! 登录与建连两个调用点,而本轮新增的队列端点带着一条**无界写入路径**上生产
//! (#109 F-003)。两套限流并存的话,迟早只改一处。
//!
//! ## 键是账号 id,不是 token
//!
//! 同一个账号可以有多个 token(多端登录、重新登录),按 token 分桶等于让总额度
//! 随 token 数线性放大 —— 而那正是要限的东西。token 也不该进限流的键或日志:
//! 它是凭据。
//!
//! 账号从 **request extensions** 里读,由前置的鉴权提取器放进去
//! (见 [`crate::gate::auth`])。不自己再解析一遍请求头,更不信任请求体里
//! 自报的账号 id。
//!
//! ## 键只能是账号 —— 这不是取舍,是那一层拿得到什么决定的
//!
//! 「同账号里一台坏设备会不会把好设备饿死?」会 —— 而且**在这一层修不了**。
//! 2026-09-21 现场问过一次,这里留一份答案,免得下一个人重新推一遍
//! (#109 F-R3)。
//!
//! 按**设备**分桶是想得到的第一个办法,但 `/signal` 的升级请求上只有一个
//! `Authorization` 头(`crates/syncplay/src/signalling.rs` 的
//! `connect_with_idle`),device id 在 `Hello` 里,而 `Hello` 是升级**之后**
//! 才发的第一条消息 —— 限流跑在升级请求上,那时它还不存在。要让它出现在
//! 升级请求里得改客户端,而会闯祸的恰恰是**改不了的旧客户端**。
//!
//! 按 IP 分同理:一个家庭 NAT 后面的几台设备,在服务端看来是同一个 IP。
//!
//! 所以这一层只有账号这一个键可用,而「一个永远连不上的端靠不停重试就能
//! 把同账号合法的那一端锁在门外」这件事,只能从别处下手:要么让那个端不再
//! 狂敲(客户端退避,见 `syncplay::client` 的 `HEALTHY_AFTER`),要么让被拒
//! 的连接不计费 —— 而后者等于开一条可以无限重试的免费通道。两条都不在
//! 这个文件里。
//!
//! ## 分组,不是一个桶
//!
//! 六组各有各的节奏。混成一个的话,一次长队列上传会把同一账号的交互操作
//! (暂停、下一首)一起饿死,而那是用户立刻看得见的卡顿。
//!
//! ## 这道闸只在**单个进程**里成立
//!
//! governor 的状态在进程内存里:N 个副本就是约 N 份预算,重启即清零。所以它
//! 的定位是**每实例削峰**,不是全局配额。跨副本严格成立的只有数据库那道
//! 队列数上限(见 `crate::store::queue::create`)。
//! 真要全局精确,那是 Redis 那一档的事,而本仓现在连第二个副本都没有。

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use governor::middleware::NoOpMiddleware;
use tower_governor::GovernorError;
use tower_governor::governor::{
    GovernorConfig, GovernorConfigBuilder,
};
use tower_governor::key_extractor::{
    KeyExtractor, SmartIpKeyExtractor,
};

use crate::store::account::Account;

/// 按**账号 id** 分桶。
///
/// 读的是 extensions 里那份已认证的 [`Account`]。取不到就说取不到 —— 那说明
/// 这条路由没有前置鉴权,是装配错了,不该悄悄退回按 IP 限。
#[derive(Clone, Copy, Debug)]
pub struct AccountKey;

impl KeyExtractor for AccountKey {
    type Key = i64;

    fn extract<T>(
        &self,
        req: &Request<T>,
    ) -> Result<Self::Key, GovernorError> {
        req.extensions()
            .get::<Account>()
            .map(|account| account.id)
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

/// 一组配置的简写。`NoOpMiddleware` = 不往响应里塞 `x-ratelimit-*` 头。
type Policy<K> = Arc<GovernorConfig<K, NoOpMiddleware>>;

/// 六条策略,各一个桶。
///
/// 构造一次、共享 `Arc`:每条路由 new 一份的话,同一个账号在两条路由上各有
/// 一份额度,而那两条本来就该共用一个桶。
#[derive(Clone)]
pub struct Policies {
    /// 建队列与发新版本。低速:一次点播一发,而人点不了那么快。
    pub queue_write: Policy<AccountKey>,
    /// 写播放意图。交互频率,**不能被大上传吃光** —— 所以与上传分开。
    pub queue_intent: Policy<AccountKey>,
    /// 执行报告。要容得下设备数乘上报频率,外加事件触发与重连时的补报。
    pub queue_report: Policy<AccountKey>,
    /// 读队列。分页读一份长队列本身就是好几发。
    pub queue_read: Policy<AccountKey>,
    /// 登录与注册。**按来源 IP**:这两条正是用来取得登录态的,那时还没有账号。
    pub auth_attempt: Policy<SmartIpKeyExtractor>,
    /// 建立信令连接。按账号 —— 连上之前已经鉴过权了。
    pub signal_connect: Policy<AccountKey>,
}

impl Policies {
    /// 按当前的正常流量定的一组数,不是库的默认值。
    ///
    /// `period` 是「多久**恢复一个**额度」,`burst_size` 是「攒得下几个」。
    /// 不用 `per_second(n)` —— 那个名字读起来像「每秒 n 次」,实际是
    /// 「每 n 秒一次」,是这套 API 上最容易写反的一处。
    pub fn tuned() -> Self {
        Self {
            // 一次点播发一条。十个的余量够连点几下与重试,而持续下来
            // 是每分钟十次 —— 人点不了这么快,机器才点得到。
            queue_write: policy(
                AccountKey,
                Duration::from_secs(6),
                10,
            ),
            // 交互:暂停、下一首、切目标。比上传宽一个档,而且**自己一个桶**,
            // 不会被一次五千首的上传把额度吃光。
            queue_intent: policy(
                AccountKey,
                Duration::from_secs(2),
                30,
            ),
            // 报告:事件驱动(换批、换歌、洗牌、回卷),不是每秒一发。
            // 但要容得下几台设备同时在线,外加重连时各补一发,所以桶开大。
            queue_report: policy(
                AccountKey,
                Duration::from_secs(1),
                120,
            ),
            // 读:一份五千首的队列要分十页读完,而换目标、重连都会重读。
            queue_read: policy(
                AccountKey,
                Duration::from_secs(1),
                60,
            ),
            // 与改之前同一个量级(原先是每分钟 20 次每 IP)。
            auth_attempt: policy(
                SmartIpKeyExtractor,
                Duration::from_secs(3),
                20,
            ),
            // 同上(原先每分钟 30 次每账号)。重连有退避,正常客户端碰不到。
            signal_connect: policy(
                AccountKey,
                SIGNAL_CONNECT_PERIOD,
                SIGNAL_CONNECT_BURST,
            ),
        }
    }

    /// 定期把久未出现的键清掉。
    ///
    /// 不清的话这几张表只涨不落 —— 账号会销号,IP 更是随便换。手写那一版
    /// 是「超过阈值顺手清一遍」,governor 这边给的是 `retain_recent`,
    /// 由调用方决定多久跑一次。
    pub fn spawn_cleanup(&self) {
        let policies = self.clone();
        tokio::spawn(async move {
            let mut tick =
                tokio::time::interval(CLEANUP_EVERY);
            loop {
                tick.tick().await;
                policies
                    .queue_write
                    .limiter()
                    .retain_recent();
                policies
                    .queue_intent
                    .limiter()
                    .retain_recent();
                policies
                    .queue_report
                    .limiter()
                    .retain_recent();
                policies
                    .queue_read
                    .limiter()
                    .retain_recent();
                policies
                    .auth_attempt
                    .limiter()
                    .retain_recent();
                policies
                    .signal_connect
                    .limiter()
                    .retain_recent();
            }
        });
    }
}

/// 建连额度多久恢复一个。拎出来是为了让下面那条「同账号几台设备掏不空它」
/// 的测试与生产用的是同一组数。
const SIGNAL_CONNECT_PERIOD: Duration =
    Duration::from_secs(2);

/// 建连额度攒得下几个。
const SIGNAL_CONNECT_BURST: u32 = 30;

/// 多久清一次过期的键。
const CLEANUP_EVERY: Duration = Duration::from_secs(300);

fn policy<K: KeyExtractor>(
    key: K,
    period: Duration,
    burst: u32,
) -> Policy<K> {
    Arc::new(
        GovernorConfigBuilder::default()
            .period(period)
            .burst_size(burst)
            .key_extractor(key)
            .finish()
            .expect("限流参数写死在代码里,建不出来是编译期就该发现的错"),
    )
}

/// 被限住时回什么。
///
/// 用仓库自己的 [`contract::ErrorDto`] 形状,带 `Retry-After` —— 客户端按
/// `code` 分支(见 `contract::ErrorDto`),而 `Retry-After` 告诉它等多久,
/// 免得它立刻重试把闸撞得更死。
///
/// **与配额耗尽分开**:那一种回 `queue_quota_exceeded`,永远不会自己好,
/// 客户端不该重试。混成同一个 code 的话,客户端会对着一个永久性的拒绝
/// 无限退避重试。
pub fn too_many_requests(error: GovernorError) -> Response {
    match error {
        GovernorError::TooManyRequests {
            wait_time,
            ..
        } => {
            // **落一行日志。** 少了它,额度耗尽在服务端这侧完全无声:
            // 客户端只看到一句 429,而「谁在消耗、消耗到什么程度」没有
            // 任何地方答得上来 —— 2026-09-21 生产上那次就是这么查不下去的
            // (#109 F-R3)。`wait_time` 是最有用的那个数:它不是「还要等
            // 两秒」,而是**欠了多少**,几百秒就说明刚才有过一场风暴。
            tracing::warn!(wait_time, "限流挡下一条请求");
            let mut response = crate::error::rate_limited()
                .into_response();
            if let Ok(value) = wait_time.to_string().parse()
            {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, value);
            }
            response
        }
        // 取不到键说明这条路由没挂前置鉴权,是装配错了。回 500 而不是放行:
        // 放行等于这条路由从此不限流,而没有人会发现。
        GovernorError::UnableToExtractKey => {
            tracing::error!(
                "限流取不到账号:这条路由没有前置鉴权,装配错了"
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(contract::ErrorDto {
                    code: "internal".to_owned(),
                    message: "内部错误".to_owned(),
                }),
            )
                .into_response()
        }
        GovernorError::Other { code, msg, .. } => (
            code,
            axum::Json(contract::ErrorDto {
                code: "internal".to_owned(),
                message: msg.unwrap_or_else(|| {
                    "内部错误".to_owned()
                }),
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use governor::clock::{Clock, FakeRelativeClock};
    use governor::{Quota, RateLimiter};

    use super::*;

    /// 生产那组数建出来的桶,钟由测试拨。
    ///
    /// 与 `tower_governor` 里那个同一个 GCRA、同一组参数;按账号分键只是
    /// 每个键各一个这样的桶,这里只看一个账号。
    fn signal_bucket(
        clock: &FakeRelativeClock,
    ) -> RateLimiter<
        governor::state::NotKeyed,
        governor::state::InMemoryState,
        FakeRelativeClock,
        NoOpMiddleware<
            <FakeRelativeClock as Clock>::Instant,
        >,
    > {
        let quota =
            Quota::with_period(SIGNAL_CONNECT_PERIOD)
                .expect("周期不为零")
                .allow_burst(
                    NonZeroU32::new(SIGNAL_CONNECT_BURST)
                        .expect("额度不为零"),
                );
        RateLimiter::direct_with_clock(quota, clock.clone())
    }

    /// 同账号两台设备一起反复重启应用,共用的建连桶也掏不空(#118 验收三)。
    ///
    /// 建连按账号一个桶,这一层拿不到设备 id(见模块头),所以「会不会一台
    /// 把另一台锁在门外」只能拿数字答:一次重启花一个额度,两秒回一个。
    /// 两台**同一时刻**一起重启、每两秒一次 —— 比人能做到的快得多(桌面冷启动
    /// 与安卓冷启动都要好几秒)—— 连着二十轮,一次都不会被挡。
    #[test]
    fn two_devices_restarting_together_never_drain_the_shared_bucket()
     {
        const ROUNDS: u32 = 20;
        let clock = FakeRelativeClock::default();
        let bucket = signal_bucket(&clock);

        for round in 0..ROUNDS {
            for device in ["pc", "phone"] {
                assert!(
                    bucket.check().is_ok(),
                    "第 {round} 轮 {device} 重启时被限流"
                );
            }
            clock.advance(SIGNAL_CONNECT_PERIOD);
        }
    }

    /// 反过来钉住桶是真的有底的:同样两台,每秒各敲一次,撑不过一分钟。
    ///
    /// 少了这一条,上面那条也可能只是因为桶大到挡不住任何东西才过的。
    #[test]
    fn a_sustained_storm_from_one_account_is_still_throttled()
     {
        let clock = FakeRelativeClock::default();
        let bucket = signal_bucket(&clock);

        let throttled_at = (0..120).find(|_| {
            let rejected = ["pc", "phone"]
                .iter()
                .any(|_| bucket.check().is_err());
            clock.advance(Duration::from_secs(1));
            rejected
        });

        assert!(
            throttled_at.is_some_and(|second| second < 60),
            "每秒两次的风暴该在一分钟内被挡,实得 {throttled_at:?}"
        );
    }
}
