//! ui 与平台媒体控件之间的契约:一份快照、一组命令,以及双方各实现一半的钩子。

use std::sync::Arc;

use app_core::LoopMode;

// 下面这些只有原生那半边用得到 —— wasm 上界面在、播放不在,那半边整个不存在
// (见下方「以下到文件末尾是原生那一半」)。

#[cfg(not(target_arch = "wasm32"))]
use app_core::{Playback, PlaybackState};
#[cfg(not(target_arch = "wasm32"))]
use slint::ComponentHandle;

#[cfg(not(target_arch = "wasm32"))]
use crate::MainWindow;
use crate::viz::CoverPixels;

/// 播放器在宿主系统眼里的样子。
///
/// 与 `PlaybackState` 不是一回事:那个记的是**装载了哪一首**,这个记的是
/// **声音走没走**。装着一首歌而没在走,在这里叫暂停,在那边没有对应的态。
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum MediaStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

/// 此刻在放什么。
///
/// 封面给两份是因为两端要的格式不同,而两份 ui 本来都攥在手里:`art_url` 是
/// 平台给的 CDN 链接,MPRIS 的 `mpris:artUrl` 直接用;`art` 是 `crate::cover`
/// 解出来喂点云的那份像素,安卓要拿它转 `Bitmap` —— 那边的 `MediaMetadata`
/// 不接受 http URL,通知栏不会替你去下图。各取一份,谁都不必再下一次。
#[derive(Clone, Default)]
pub struct NowPlaying {
    pub status: MediaStatus,
    /// 曲目身份。停下来时是空串 —— 见 [`NowPlaying::render`]。
    pub track_id: String,
    pub title: String,
    /// 列表原样给出,不 join:拼成一句是界面的选择,不是数据的。
    pub artists: Vec<String>,
    pub duration_ms: i64,
    pub art_url: Option<String>,
    pub art: Option<Arc<CoverPixels>>,
    /// 随机开着没有。
    ///
    /// 与上面那些不同,它**不是曲目的属性,是播放器的** —— MPRIS 把 `Shuffle`
    /// 挂在 Player 接口上,安卓的 `MediaSession` 也一样。所以队列放完、
    /// 曲目那半清空之后,这一位照旧要如实报出去。
    pub shuffle: bool,
    /// 循环三态。与随机同一个理由:播放器的属性,曲目清空后照旧报。
    /// MPRIS 那边对应 `LoopStatus`,安卓对应 `REPEAT_MODE`。
    pub loop_mode: LoopMode,
}

// —— 以下到文件末尾是原生那一半 ——
//
// wasm 上界面在、播放不在(`music::bind` 是个桩子),于是这些没有任何调用者。
// 留着不 cfg 掉就是一片死代码警告,而那正是「这半边只对原生成立」该说的话。

#[cfg(not(target_arch = "wasm32"))]
impl NowPlaying {
    /// 从播放状态渲染出来。
    ///
    /// `playing` 要另给:`PlaybackState` 说不出暂停(理由见 [`MediaStatus`])。
    ///
    /// 停下来时返回的是**空的**,而不是留着最后那一首。留着就是投影过期的老毛病
    /// 换了个地方犯 —— 队列放完之后锁屏上仍挂着一首没在放的歌,没有任何东西
    /// 会去把它收掉(见 `crate::notice` 的模块注释)。
    pub(crate) fn render(
        state: &PlaybackState,
        playing: bool,
        shuffle: bool,
        loop_mode: LoopMode,
        art: Option<Arc<CoverPixels>>,
    ) -> Self {
        let track = match state {
            PlaybackState::Loading(track)
            | PlaybackState::Playing(track) => track,
            PlaybackState::Idle
            | PlaybackState::Failed(_) => {
                // 曲目那半清空,随机与循环照旧报 —— 它们是播放器的开关,
                // 不随「这一刻装没装着歌」一起没。
                return Self {
                    shuffle,
                    loop_mode,
                    ..Self::default()
                };
            }
        };

        Self {
            shuffle,
            loop_mode,
            status: if playing {
                MediaStatus::Playing
            } else {
                MediaStatus::Paused
            },
            track_id: track.id.clone(),
            title: track.title.clone(),
            artists: track.artists.clone(),
            duration_ms: track.duration_ms,
            art_url: track.cover.clone(),
            art,
        }
    }

    /// 去重用的指纹。
    ///
    /// 封面的**有无**要算进来:换歌那一刻图还在路上,第一份必然没有图。只认
    /// id 的话,图到了也推不出去,控件上就永远是空封面。有无就够了 —— 同一首歌
    /// 不会换第二张图。
    pub(super) fn fingerprint(
        &self,
    ) -> (MediaStatus, &str, bool, bool, LoopMode) {
        (
            self.status,
            &self.track_id,
            self.art.is_some(),
            // 只拨了随机或循环的那一次,歌与状态一个字都没变。不认它的话
            // 去重会把这次变更整个吃掉,外面那个开关就一直停在旧样子。
            self.shuffle,
            self.loop_mode,
        )
    }
}

/// 宿主系统按下的键。
///
/// `Play` / `Pause` / `Toggle` 三个都留着:外面确实是三个键,而界面只有一个
/// 切换回调,那道翻译在 [`toggles`] 里做 —— 让后端去猜「现在是不是在放」,
/// 两个后端迟早有一个猜错。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    /// 跳到绝对位置,毫秒。
    SeekTo(i64),
    /// 相对当前位置跳,毫秒,可负。MPRIS 的 `Seek` 是这一种。
    SeekBy(i64),
    /// 随机开关拨到这个值。外面给的是**绝对值**,而界面只有一个切换回调,
    /// 那道翻译在 [`flips_shuffle`] 里做 —— 与 [`toggles`] 同一个理由。
    SetShuffle(bool),
    /// 循环拨到这个态。同样是绝对值,与现值的比对在 [`wants_loop`] 里做。
    SetLoop(LoopMode),
}

/// ui 交给后端的两根线。
///
/// 后端在自己的线程上(zbus 的执行器、安卓的主 looper)随时可以拉位置、发命令;
/// `command` 内部负责跳回 Slint 的事件循环,后端不必知道有这回事。
#[derive(Clone)]
pub struct MediaHooks {
    pub command: Arc<dyn Fn(MediaCommand) + Send + Sync>,
    pub position:
        Arc<dyn Fn() -> core::time::Duration + Send + Sync>,
}

/// 一端的系统媒体控件。平台入口 crate 实现它。
pub trait MediaControls {
    /// 把此刻在放的东西报出去。
    fn publish(&self, now: &NowPlaying);
}

/// 什么都不做的那一份。
///
/// 给还没有实现的端用(web、iOS、Windows、macOS),以及本地跑测试时。
pub struct NoControls;

impl MediaControls for NoControls {
    fn publish(&self, _now: &NowPlaying) {}
}
