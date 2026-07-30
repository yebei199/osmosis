//! 播放一首歌的状态模型。
//!
//! 点一首歌到出声之间有**两次**等待:先向服务端要一条直链,再把流打开、解码。
//! 所以"正在加载"不是可有可无的中间态,它是用户按下去之后看到的那几秒。
//!
//! 与 [`crate::Health`] 同一套代际(generation)机制:连点两首歌时,先发出的
//! 那次请求可能后返回,不许它盖掉后点的那首。

use core::fmt;
use std::cell::RefCell;
use std::future::Future;

use contract::TrackDto;

/// 播放的可观测状态。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PlaybackState {
    /// 没在放,也没在准备。
    #[default]
    Idle,
    /// 正在为某首歌准备播放 —— 取直链、开流、解码都算这一段。
    Loading(TrackDto),
    /// 出声了。
    Playing(TrackDto),
    /// 没放成,附上人能读的原因。
    ///
    /// 不静默回 [`Self::Idle`]:那样用户点了一下什么都没发生,无从判断是
    /// 自己没点中还是歌放不了。
    Failed(String),
}

/// 播放状态及其代际。
#[derive(Debug, Default)]
pub struct Playback {
    state: PlaybackState,
    generation: u64,
}

impl Playback {
    pub fn state(&self) -> &PlaybackState {
        &self.state
    }

    /// 开始准备一首歌:进入 [`PlaybackState::Loading`],返回本次的代号。
    fn begin(&mut self, track: TrackDto) -> u64 {
        self.generation += 1;
        self.state = PlaybackState::Loading(track);
        self.generation
    }

    /// 结束一次准备。代号过期则丢弃结果、状态不变,返回 `false`。
    fn finish(
        &mut self,
        generation: u64,
        result: Result<TrackDto, String>,
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.state = match result {
            Ok(track) => PlaybackState::Playing(track),
            Err(message) => PlaybackState::Failed(message),
        };
        true
    }

    /// 这个代号是不是还是当前的。
    fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    /// 停止播放,回到 [`PlaybackState::Idle`]。
    ///
    /// 同时作废当前代际:停止之后,正在路上的那次准备回来了也不该出声。
    pub fn stop(&mut self) {
        self.generation += 1;
        self.state = PlaybackState::Idle;
    }
}

/// 播放一首歌,把结果写回 `playback`。
///
/// 分两步注入,而不是一个"从取直链一路做到出声"的闭包:
///
/// - `prepare` 是**慢**的那一半(取直链、开流、解码),可以 await;
/// - `commit` 是**不可撤销**的那一半(把源塞进播放器),同步、立刻生效。
///
/// 拆开是为了在两者之间验一次代际。合成一步的话,一次过期的准备回来时
/// 照样会 `player.play()`,把后点的那首顶掉 —— 状态文字写着 B,耳朵听到的
/// 是 A,而且不会有任何报错。
///
/// 生产环境的 `prepare` 走 `api` + `audio`,`commit` 交给 `audio::Player`;
/// 测试里两个都传预置闭包。本 crate 因此既不依赖 `api` 也不依赖 `audio`。
pub async fn play<Prepare, Fut, Commit, Ready, Error>(
    playback: &RefCell<Playback>,
    track: TrackDto,
    prepare: Prepare,
    commit: Commit,
) where
    Prepare: FnOnce(TrackDto) -> Fut,
    Fut: Future<Output = Result<Ready, Error>>,
    Commit: FnOnce(Ready),
    Error: fmt::Display,
{
    // 借用必须在 await 之前归还,否则同一时刻的第二次 play 会 panic。
    let generation =
        playback.borrow_mut().begin(track.clone());
    let prepared = prepare(track.clone())
        .await
        .map_err(|error| error.to_string());

    // 准备期间用户可能又点了别的歌。过期的这次连播放器都不许碰,
    // 备好的源就地丢掉。
    let still_current =
        playback.borrow().is_current(generation);
    if !still_current {
        return;
    }

    let result = prepared.map(|ready| {
        commit(ready);
        track
    });
    playback.borrow_mut().finish(generation, result);
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use super::*;

    /// 把一个 future 跑到完成。
    ///
    /// ponytail: 忙等轮询,只对"立即就绪"的 future 有意义 —— 测试里注入的
    /// 闭包正是如此。与 `health.rs` 的同名函数重复一份,好过为两行代码
    /// 造一个测试工具模块。
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

    fn track(id: &str) -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: id.to_owned(),
            title: format!("歌 {id}"),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 234_000,
        }
    }

    /// 按下去之后必须先进 Loading:那几秒里 UI 要能显示"正在加载",
    /// 否则用户会以为没点中而反复点。
    #[test]
    fn play_enters_loading_then_playing() {
        let playback = RefCell::new(Playback::default());

        // 手动走一遍 begin/finish,好在两步之间观察中间态 ——
        // play() 内部的 await 在测试里是立即就绪的,拦不住。
        let generation =
            playback.borrow_mut().begin(track("1"));
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Loading(track("1"))
        );

        playback
            .borrow_mut()
            .finish(generation, Ok(track("1")));
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Playing(track("1"))
        );
    }

    /// 连点两首:先点的那次后返回,不许盖掉后点的。
    ///
    /// 没有代际机制的话,状态会停在第一首上,而声音放的是第二首 ——
    /// 界面与耳朵不一致,且不会有任何报错。
    #[test]
    fn stale_response_does_not_override_newer_request() {
        let playback = RefCell::new(Playback::default());

        let first = playback.borrow_mut().begin(track("1"));
        let second =
            playback.borrow_mut().begin(track("2"));

        let accepted = playback
            .borrow_mut()
            .finish(first, Ok(track("1")));
        assert!(!accepted, "过期的结果不该被接受");
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Loading(track("2")),
            "状态不该被过期结果改动"
        );

        assert!(
            playback
                .borrow_mut()
                .finish(second, Ok(track("2")))
        );
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Playing(track("2"))
        );
    }

    /// 过期的加载不提交给播放器。
    ///
    /// 代际此前只挡住状态文字,声音照出 —— 先点 A 后点 B,A 慢一步回来
    /// 仍会把 B 从播放器里顶掉,耳朵听到的是 A 而界面写着 B。
    #[test]
    fn a_superseded_load_is_not_committed() {
        let playback = RefCell::new(Playback::default());
        let committed = Cell::new(0);

        // 准备期间用户又点了另一首 —— 本次就此过期。借用在 await 前已归还,
        // 所以这里能再 borrow_mut(这正是 `play` 那句注释守着的性质)。
        block_on(play(
            &playback,
            track("1"),
            |_| async {
                playback.borrow_mut().begin(track("2"));
                Ok::<_, &str>(())
            },
            |()| committed.set(committed.get() + 1),
        ));

        assert_eq!(committed.get(), 0, "过期的那次不该出声");
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Loading(track("2")),
            "状态也不该被过期结果改动"
        );
    }

    /// 没被顶掉的那次照常提交,状态进 Playing。
    #[test]
    fn the_current_load_is_committed() {
        let playback = RefCell::new(Playback::default());
        let committed = Cell::new(0);

        block_on(play(
            &playback,
            track("1"),
            |_| async { Ok::<_, &str>(()) },
            |()| committed.set(committed.get() + 1),
        ));

        assert_eq!(committed.get(), 1);
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Playing(track("1"))
        );
    }

    /// 准备阶段就失败:不提交,状态进 Failed 并留下原因。
    #[test]
    fn a_failed_preparation_reports_failure_without_committing()
    {
        let playback = RefCell::new(Playback::default());
        let committed = Cell::new(0);

        block_on(play(
            &playback,
            track("1"),
            |_| async { Err::<(), _>("直链已过期") },
            |()| committed.set(committed.get() + 1),
        ));

        assert_eq!(committed.get(), 0);
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Failed("直链已过期".to_owned())
        );
    }

    /// 停止回到 Idle,并且作废正在路上的那次准备 ——
    /// 否则用户按了停止,几秒后声音又自己响起来。
    #[test]
    fn stop_returns_to_idle() {
        let playback = RefCell::new(Playback::default());

        let generation =
            playback.borrow_mut().begin(track("1"));
        playback.borrow_mut().stop();
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Idle
        );

        let accepted = playback
            .borrow_mut()
            .finish(generation, Ok(track("1")));
        assert!(!accepted, "停止后到达的结果不该让它出声");
        assert_eq!(
            playback.borrow().state(),
            &PlaybackState::Idle
        );
    }
}
