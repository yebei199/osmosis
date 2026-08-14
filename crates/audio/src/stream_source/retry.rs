//! 跳转的执行与重试:上游拒绝或卡住时,退一秒再试一次,再不行就认输。

use std::sync::mpsc;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{Sample, Source};

use crate::codec::{SYNC_CHANNELS, SYNC_SAMPLE_RATE};

use super::{SEEK_RETRY_BACKOFF, SeekRequest, SeekState};

/// 执行一次跳转,交回此后该用的通道。
///
/// 失败不结束这条线程:跳不了的格式照样能从当前位置接着放,
/// 把歌掐掉是比"跳不动"更糟的答复。
///
/// 结论走**两条路中的一条,不是两条**:裁决送得进去说明调用方还在等,它自己
/// 会报;送不进去说明它已经超时走了,那才留在 [`SeekState`] 上等界面来取。
/// 两条都走的话,同一次失败会被说两遍 —— 一遍来自拖动那一下,一遍来自一秒后
/// 的轮询。
pub(super) fn apply_seek<S: Source>(
    source: &mut S,
    (to, fresh, verdict): SeekRequest,
    state: &SeekState,
) -> mpsc::SyncSender<Sample> {
    let outcome = seek_with_retry(source, to);

    if let Err(err) = &outcome {
        // 摊开整条因果链:最外面那句往往是「解码器报错了」,等于没说
        log::warn!(
            "跳转到 {to:?} 失败: {}",
            crate::full_cause(err)
        );
    }

    let why = outcome
        .as_ref()
        .err()
        .map(|err| crate::full_cause(err));

    // 先落状态,再把裁决交出去。反过来的话,调用方一拿到答复就会去问
    // `is_seeking`,而那时这条线程还没走到下面 —— 谁先跑到纯看调度,
    // 开发机上从没输过,两核的 CI runner 上输了。
    state.finish();

    // send 失败 = 接收端已经丢了 = 调用方超时走了,它那句失败没人听见,
    // 于是留在状态上等界面来取。送进去了就不再说第二遍(见上面的注释)。
    //
    // 这中间有一小段状态是 Idle 而非 Failed。界面一秒问一次,撞进这几微秒
    // 只是晚一秒看到那句话,而失败本身留在状态上,不会丢。
    if verdict.send(outcome).is_err()
        && let Some(why) = why
    {
        state.fail(why);
    }

    fresh
}

/// 跳到 `to`;落点跳不动就往前挪 [`SEEK_RETRY_BACKOFF`] 再试一次。
///
/// 重试成功后**向前解码丢弃**到 `to`,位置因此停在调用方要的那一刻 ——
/// rodio 的 `TrackPosition` 拿到 `Ok` 就把位置设成目标值,落点若真在目标
/// 之前,进度条会一直偏着直到下次跳转,歌词跟着一起偏。丢掉的这一段顺带
/// 把比特池填满,正是回退想要的东西。
pub(super) fn seek_with_retry<S: Source>(
    source: &mut S,
    to: Duration,
) -> Result<(), SeekError> {
    let stuck = match source.try_seek(to) {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };

    let earlier = to.saturating_sub(SEEK_RETRY_BACKOFF);
    // 已经在开头了,退不出一个新的落点来
    if earlier == to {
        return Err(stuck);
    }

    log::warn!(
        "跳转到 {to:?} 落点解不开({}),回退到 {earlier:?} 重试",
        crate::full_cause(&stuck)
    );
    source.try_seek(earlier)?;
    discard(source, to - earlier);
    Ok(())
}

/// 向前解码并丢弃 `span` 这么长的采样。
///
/// 采样率与声道数取本模块的常量而不去问 `source`:[`buffered`] 的前提就是
/// 它跑在 [`crate::codec::normalize`] 之后,那一层的全部职责就是把这两样
/// 拉成 48kHz 立体声。
pub(super) fn discard<S: Source>(
    source: &mut S,
    span: Duration,
) {
    let per_second = f64::from(SYNC_SAMPLE_RATE)
        * f64::from(SYNC_CHANNELS);
    let count = (span.as_secs_f64() * per_second) as u64;
    for _ in 0..count {
        if source.next().is_none() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
