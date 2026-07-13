//! 服务端健康状态的三态模型。
//!
//! 这是本项目里第一个"请求进行中"的状态。它演示了 `app-core` 如何在不知道
//! HTTP 存在的前提下,完整地建模一次网络往返的生命周期。

use core::fmt;
use std::cell::RefCell;
use std::future::Future;

use contract::HealthDto;

/// 一次健康检查的可观测状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HealthState {
    /// 还没查过。
    #[default]
    Idle,
    /// 查询进行中。
    Loading,
    /// 查到了。
    Loaded(HealthDto),
    /// 查失败了,附上人类可读的原因。
    ///
    /// ponytail: 存字符串而非错误枚举 —— UI 只是把它显示出来。等到需要按错误
    /// 种类分支(比如超时才重试)时,再把它换成枚举。
    Failed(String),
}

/// 健康状态,以及"哪一次请求的结果才算数"的判据。
#[derive(Debug, Default)]
pub struct Health {
    /// 每发起一次请求便加一。
    ///
    /// 网络响应可以乱序返回:用户连点两次,first_request的请求可能后到。带上代号,
    /// 就能在结果回来时判断它是否已被更晚的请求取代。
    generation: u64,
    state: HealthState,
}

impl Health {
    /// 当前状态。
    pub fn state(&self) -> &HealthState {
        &self.state
    }

    /// 开始一次新请求:进入 [`HealthState::Loading`],返回本次请求的代号。
    fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.state = HealthState::Loading;
        self.generation
    }

    /// 结束一次请求。
    ///
    /// 若 `generation` 已被更晚的请求取代,结果会被丢弃、状态不变,返回 `false`。
    fn finish(
        &mut self,
        generation: u64,
        result: Result<HealthDto, String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.state = match result {
            Ok(dto) => HealthState::Loaded(dto),
            Err(message) => HealthState::Failed(message),
        };
        true
    }
}

/// 拉取一次服务端健康状态,把结果写回 `health`。
///
/// `fetch` 由调用方注入 —— 生产环境传 `api::health`,测试里传一个返回预置结果
/// 的闭包。因此本 crate 既不依赖 `api`,也不依赖网络。
///
/// 返回的 future 不要求 `Send`:它由 UI 线程上的 `spawn_local` 驱动。
/// 见 `docs/adr/0002`。
pub async fn refresh<Fetch, Fut, Error>(
    health: &RefCell<Health>,
    fetch: Fetch,
) where
    Fetch: FnOnce() -> Fut,
    Fut: Future<Output = Result<HealthDto, Error>>,
    Error: fmt::Display,
{
    // 借用必须在 await 之前归还,否则同一时刻的第二次 refresh 会 panic。
    let generation = health.borrow_mut().begin();
    let result =
        fetch().await.map_err(|error| error.to_string());
    health.borrow_mut().finish(generation, result);
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use super::*;

    /// 把一个 future 跑到完成。
    ///
    /// ponytail: 忙等轮询,只对"立即就绪"的 future 有意义 —— 测试里注入的
    /// 闭包正是如此。不引入 executor 依赖。真需要挂起时换成 `futures::executor`。
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = pin!(future);
        let mut context =
            Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(value) =
                future.as_mut().poll(&mut context)
            {
                return value;
            }
        }
    }

    fn dto() -> HealthDto {
        HealthDto {
            status: "ok".to_owned(),
            protocol_version: 1,
        }
    }

    /// 没查过时是 Idle。
    #[test]
    fn initial_state_is_idle() {
        assert_eq!(
            *Health::default().state(),
            HealthState::Idle
        );
    }

    /// 成功路径:请求返回 DTO,状态变成 Loaded。
    #[test]
    fn refresh_success_enters_loaded() {
        let health = RefCell::new(Health::default());
        block_on(refresh(&health, || async {
            Ok::<_, String>(dto())
        }));
        assert_eq!(
            *health.borrow().state(),
            HealthState::Loaded(dto())
        );
    }

    /// 失败路径:请求返回错误,状态变成 Failed 并带上原因。
    #[test]
    fn refresh_failure_enters_failed() {
        let health = RefCell::new(Health::default());
        block_on(refresh(&health, || async {
            Err::<HealthDto, _>("connection refused")
        }));
        assert_eq!(
            *health.borrow().state(),
            HealthState::Failed(
                "connection refused".to_owned()
            )
        );
    }

    /// 请求期间状态是 Loading。
    #[test]
    fn begin_sets_state_loading() {
        let mut health = Health::default();
        health.begin();
        assert_eq!(*health.state(), HealthState::Loading);
    }

    /// 边界:first_request的请求后到时,其结果必须被丢弃 —— 否则会覆盖更新的数据。
    #[test]
    fn stale_response_is_discarded() {
        let mut health = Health::default();
        let first_request = health.begin();
        let second_request = health.begin();

        // first_request的请求最后才返回。
        assert!(!health.finish(
            first_request,
            Err("timed out".to_owned())
        ));
        assert_eq!(*health.state(), HealthState::Loading);

        // second_request的请求才是唯一算数的那个。
        assert!(health.finish(second_request, Ok(dto())));
        assert_eq!(
            *health.state(),
            HealthState::Loaded(dto())
        );
    }
}
