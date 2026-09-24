//! 播放上报:一次真的出了声的播放,报给服务端记进历史。

use super::*;

/// 起播上报的判据:声音真的出来那一刻报一次,同一次播放不重复报。
///
/// `last` 是上一次记住的身份,由调用方跨帧持有。返回 `Some` 就是这一帧要报的
/// 那一首,身份取 (平台, 平台内 id) —— 歌曲的身份本来就是这一对(contract)。
///
/// - `Playing` 且与 `last` 不同:报,并记住它;
/// - `Playing` 且与 `last` 相同:不报。轮询每秒经过一次,不去重的话一首三分钟
///   的歌会报出一百八十次播放;
/// - 其余状态:不报,并把 `last` 清掉。清掉是为了「重放同一首」—— 重新点会先
///   经过 `Loading`,不清的话单曲循环整晚只记一次,而它确实放了一整晚。
///
/// 只认 `Playing` 就等于只认「出声了」:取流失败停在 `Failed`,准备期间被顶掉的
/// 那次连状态都没换(`app_core::play` 的代际校验),两者都到不了这里。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn play_to_report(
    state: &PlaybackState,
    last: &mut Option<(String, String)>,
) -> Option<(String, String)> {
    let PlaybackState::Playing(track) = state else {
        *last = None;
        return None;
    };
    let now = (track.platform.clone(), track.id.clone());
    if last.as_ref() == Some(&now) {
        return None;
    }
    *last = Some(now.clone());
    Some(now)
}

/// 把一首歌准备到「随时能出声」为止:取直链 → 开流 → 解码。**慢**的那一半。
///
/// 这是注入给 `app_core::play` 的 `prepare`。`app-core` 只看到"一个返回 Result
/// 的 future",看不到 HTTP,也看不到 alsa。
///
/// 停在解码,不往下走:再往下就是把源塞进播放器,那一步不可撤销。中间隔着
/// 一次代际校验 —— 准备期间被顶掉的这一份就地丢掉(见 `app_core::play`)。
#[cfg(not(target_arch = "wasm32"))]
///
/// `notice` 是要不要把失败说给用户听:点播给一个窗口句柄,预取给 `None`
/// —— 预取失败不声张(见 [`super::playback::advance::start_prefetch`])。
pub(super) async fn prepare(
    player: Arc<Result<audio::Player, audio::AudioError>>,
    notice: Option<slint::Weak<MainWindow>>,
    track: TrackDto,
) -> Result<(audio::Loaded, audio::StreamHealth), String> {
    // 没声卡就在这里认输,别等下载完才发现放不了。
    if let Err(error) = player.as_ref() {
        return Err(error.to_string());
    }

    let source = match api::play_source(&track.id).await {
        Ok(source) => source,
        Err(error) => {
            // 状态行那句话点不了,而这一种失败的解法是去个人页扫码 ——
            // 所以额外弹一条带去处的通知。别的失败不弹:状态行已经说过
            // 一遍,再弹一条只是同一件事说两次。
            if crate::pages::account::netease_unbound(
                &error,
            ) && let Some(ui) =
                notice.and_then(|weak| weak.upgrade())
            {
                crate::pages::account::report_failure(
                    &ui,
                    "点播失败",
                    &error,
                );
            }

            return Err(
                crate::pages::account::request_failure_text(
                    &error,
                ),
            );
        }
    };
    // 开流与解码都在 `audio` 自己的后台 runtime 上跑 —— 这里是 Slint 的 UI 线程,
    // 没有 tokio 反应堆,也不能被阻塞读占住。
    audio::load(&source.url)
        .await
        .map_err(|error| error.to_string())
}

/// 把备好的源交给播放器。**不可撤销**的那一半,同步、立刻生效。
///
/// 无声卡时这里什么都不做 —— 那种情况 [`prepare`] 已经先报了错,走不到这里。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn emit(
    player: &Arc<Result<audio::Player, audio::AudioError>>,
    stream: &Rc<RefCell<Option<audio::StreamHealth>>>,
    seeking: &Rc<RefCell<Option<audio::SeekState>>>,
    decoded: audio::Loaded,
    health: audio::StreamHealth,
    start: Option<Start>,
) {
    use audio::buffered;
    use audio::pcm::normalize;

    let Ok(player) = player.as_ref() else { return };
    // 换歌即换证据。上一首的死亡证明留着的话,新歌一放空就会被误报成断流。
    stream.borrow_mut().replace(health);
    // 先归一再缓冲,顺序是硬的:`buffered` 交出的源对外声称 48kHz 立体声,
    // 格式得先对上;缓冲把解码挪到自己的线程,声卡回调从此不碰网络(见
    // `audio::buffered`)。少了这一层,网络抖一下就是设备欠载。
    let source = buffered(normalize(decoded));
    // 跳转状态得在源被交出去之前取走:此后它归 rodio,外面再也够不着。
    seeking.borrow_mut().replace(source.seek_state());
    match start {
        None => player.play(source),
        // 迁移过来的那一首从锚点接着放(#137 ③)。跳不动就停在暂停上、说一句,
        // 不从 0:00 放起来 —— 那会让用户把整首从头再听一遍,还以为是迁移成功了。
        Some(start) => {
            if let Err(error) = player.play_from(
                source,
                start.at,
                start.playing,
            ) {
                log::warn!(
                    "迁移过来的那一首跳不到 {:?}: {error}",
                    start.at
                );
            }
        }
    }
}

/// 一首歌不从头放时,从哪、放不放。迁移过来的那一首用它(#137 ③)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Start {
    pub(super) at: core::time::Duration,
    pub(super) playing: bool,
}
