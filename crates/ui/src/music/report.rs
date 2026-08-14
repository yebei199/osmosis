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
/// 的 future",看不到 HTTP、alsa,也看不到 WebRTC。
///
/// 停在解码,不往下走:再往下就是把源塞进播放器,那一步不可撤销。中间隔着
/// 一次代际校验 —— 准备期间被顶掉的这一份就地丢掉(见 `app_core::play`)。
#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn prepare(
    player: Arc<Result<audio::Player, audio::AudioError>>,
    track: TrackDto,
) -> Result<(audio::Loaded, audio::StreamHealth), String> {
    // 没声卡就在这里认输,别等下载完才发现放不了。
    if let Err(error) = player.as_ref() {
        return Err(error.to_string());
    }

    let source = api::play_source(&track.id)
        .await
        .map_err(|error| error.to_string())?;
    // 开流与解码都在 `audio` 自己的后台 runtime 上跑 —— 这里是 Slint 的 UI 线程,
    // 没有 tokio 反应堆,也不能被阻塞读占住。
    audio::load(&source.url)
        .await
        .map_err(|error| error.to_string())
}

/// 把备好的源交给播放器与同播。**不可撤销**的那一半,同步、立刻生效。
///
/// 每首歌都分一支给同播,不管当下有没有人在听:支路满了会自己丢采样
/// (见 `audio::codec::Tee`),而等"确认有人听"再接的话,换歌时听众会掉音。
///
/// 无声卡时这里什么都不做 —— 那种情况 [`prepare`] 已经先报了错,走不到这里。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn emit(
    player: &Arc<Result<audio::Player, audio::AudioError>>,
    sync: &crate::syncplay::Sync,
    stream: &Rc<RefCell<Option<audio::StreamHealth>>>,
    seeking: &Rc<RefCell<Option<audio::SeekState>>>,
    decoded: audio::Loaded,
    health: audio::StreamHealth,
) {
    use audio::buffered;
    use audio::codec::{BRANCH_CAPACITY, Tee, normalize};

    let Ok(player) = player.as_ref() else { return };
    // 换歌即换证据。上一首的死亡证明留着的话,新歌一放空就会被误报成断流。
    stream.borrow_mut().replace(health);
    // 先归一再缓冲再分支,三步的顺序都是硬的:
    //
    // - 归一在最前:`buffered` 交出的源对外声称 48kHz 立体声,格式得先对上;
    // - 缓冲在中间:它把解码挪到自己的线程,声卡回调从此不碰网络(见
    //   `audio::buffered`)。少了这一层,网络抖一下就是设备欠载;
    // - 分支在最后:本机听到的和推给听众的因此仍是同一批采样。
    let source = buffered(normalize(decoded));
    // 跳转状态得在源被交出去之前取走:此后它归 rodio,外面再也够不着。
    seeking.borrow_mut().replace(source.seek_state());
    let (tee, branch) = Tee::new(source, BRANCH_CAPACITY);
    // 先换歌再交支路。反过来的话,新泵在上一首还没被丢掉时就起来了,
    // 两条泵会同时往同一条轨上写,听众听到的是两首歌交错的几十毫秒。
    player.play(tee);
    sync.feed(branch);
}
