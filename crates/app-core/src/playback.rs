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
/// `start` 由调用方注入 —— 生产环境传一个"取直链 + 开流 + 送进 audio::Player"
/// 的闭包,测试里传一个返回预置结果的闭包。本 crate 因此既不依赖 `api`
/// 也不依赖 `audio`。
pub async fn play<Start, Fut, Error>(
    playback: &RefCell<Playback>,
    track: TrackDto,
    start: Start,
) where
    Start: FnOnce(TrackDto) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
    Error: fmt::Display,
{
    // 借用必须在 await 之前归还,否则同一时刻的第二次 play 会 panic。
    let generation =
        playback.borrow_mut().begin(track.clone());
    let result = start(track.clone())
        .await
        .map(|()| track)
        .map_err(|error| error.to_string());
    playback.borrow_mut().finish(generation, result);
}

#[cfg(test)]
mod tests {
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

    /// 失败要留下原因。走完整的 `play` 路径,顺带验证错误被 `Display` 成了字符串。
    #[test]
    fn failed_playback_keeps_reason() {
        let playback = RefCell::new(Playback::default());

        block_on(play(&playback, track("1"), |_| async {
            Err::<(), _>("直链已过期")
        }));

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
