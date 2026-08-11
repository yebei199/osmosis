//! 系统媒体控件的接缝。
//!
//! 宿主系统各有各的说法 —— Linux 上是 session bus 的 `org.mpris.MediaPlayer2.*`,
//! 安卓上是 `MediaSession` 加一条前台通知 —— 但要的东西是同一样:此刻在放什么,
//! 以及外面按下的键该落到哪。这里定义那个共同的形状,真正说各自方言的代码住在
//! 平台入口 crate(`apps/desktop`、`apps/android`),理由见 `docs/adr/0020`。
//!
//! 界面层这一侧多做一步、后端少做一步:`Play` 与 `Toggle` 的区分、相对跳转换成
//! 绝对位置、绝对位置换成 Slint 要的比例,全在这里做完。后端因此不必记住任何
//! 状态 —— 两个后端各记一份「现在是不是在放」,迟早会有一份是错的。

use std::sync::Arc;

use app_core::LoopMode;

// 下面这些只有原生那半边用得到 —— wasm 上界面在、播放不在,那半边整个不存在
// (见下方「以下到文件末尾是原生那一半」)。
#[cfg(not(target_arch = "wasm32"))]
use core::cell::RefCell;

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
    fn fingerprint(
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

/// 界面这一侧的媒体控件把手:后端,加上推给它的那些东西的最新一份。
///
/// 去重放在这里而不是各个后端里:推送搭的是 1Hz 的续播轮询,不去重的话一首
/// 四分钟的歌会往外发两百多次状态变更,而内容一个字都没变。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Bridge {
    controls: Box<dyn MediaControls>,
    last: RefCell<Option<NowPlaying>>,
    /// 当前这一首的封面。晚于歌名到达(要过一趟网络),所以单独存。
    art: RefCell<Option<Arc<CoverPixels>>>,
    /// 当前这一首的时长,毫秒。跳转要靠它把绝对位置换成比例,而那道换算发生在
    /// 后端线程送来的命令里 —— 跨线程,所以是 `Arc<AtomicI64>` 而不是 `RefCell`。
    ///
    /// 由外面造好再交进来:命令闭包得在后端存在之前就捏好(它要被交给后端),
    /// 而后端造出来之后才有这个 `Bridge`。
    duration_ms: Arc<core::sync::atomic::AtomicI64>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Bridge {
    pub(crate) fn new(
        controls: Box<dyn MediaControls>,
        duration_ms: Arc<core::sync::atomic::AtomicI64>,
    ) -> Self {
        Self {
            controls,
            last: RefCell::new(None),
            art: RefCell::new(None),
            duration_ms,
        }
    }

    /// 换歌了,封面重新开始等。
    pub(crate) fn clear_art(&self) {
        *self.art.borrow_mut() = None;
    }

    /// 封面到了。
    pub(crate) fn set_art(&self, art: Arc<CoverPixels>) {
        *self.art.borrow_mut() = Some(art);
    }

    pub(crate) fn art(&self) -> Option<Arc<CoverPixels>> {
        self.art.borrow().clone()
    }

    /// 推出去,除非跟上次一模一样。
    pub(crate) fn publish(&self, now: NowPlaying) {
        self.duration_ms.store(
            now.duration_ms,
            core::sync::atomic::Ordering::Relaxed,
        );

        let mut last = self.last.borrow_mut();
        if last.as_ref().is_some_and(|last| {
            last.fingerprint() == now.fingerprint()
        }) {
            return;
        }

        self.controls.publish(&now);
        *last = Some(now);
    }
}

/// 这个键该不该翻转播放状态。
///
/// 界面只有一个「切换」回调。把 `Play` 一律当成切换,正在放的歌会被按停 ——
/// 锁屏上最容易误触的就是它。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn toggles(
    command: MediaCommand,
    playing: bool,
) -> bool {
    match command {
        MediaCommand::Toggle => true,
        MediaCommand::Play => !playing,
        MediaCommand::Pause => playing,
        _ => false,
    }
}

/// 这个键该不该翻转随机开关。
///
/// 与 [`toggles`] 同一道翻译:外面给的是绝对值,界面只有一个切换回调。
/// 值本来就一样还去调一次,开关会翻到反面 —— 而按下它的人什么都没要求。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn flips_shuffle(
    command: MediaCommand,
    on: bool,
) -> bool {
    matches!(command, MediaCommand::SetShuffle(want) if want != on)
}

/// 这个键要把循环拨到哪一态。与 [`flips_shuffle`] 同一道翻译:
/// 值本来就一样就不折腾 —— 按下它的人什么都没要求。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn wants_loop(
    command: MediaCommand,
    current: LoopMode,
) -> Option<LoopMode> {
    match command {
        MediaCommand::SetLoop(want) if want != current => {
            Some(want)
        }
        _ => None,
    }
}

/// slint 侧的循环三态镜像:0 关,1 列表,2 单曲。
/// builtin 之外的枚举同样过不了 seam,int 是那根线的形状。
pub(crate) fn loop_index(mode: LoopMode) -> i32 {
    match mode {
        LoopMode::Off => 0,
        LoopMode::All => 1,
        LoopMode::One => 2,
    }
}

/// [`loop_index`] 的反向。界面写坏了也不 panic,当作关。
pub(crate) fn loop_from_index(index: i32) -> LoopMode {
    match index {
        1 => LoopMode::All,
        2 => LoopMode::One,
        _ => LoopMode::Off,
    }
}

/// 这个键要跳到的绝对位置,毫秒。不是跳转键就没有答案。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn seek_target(
    command: MediaCommand,
    position_ms: i64,
) -> Option<i64> {
    match command {
        MediaCommand::SeekTo(at) => Some(at.max(0)),
        // 往回跳过了头就落到开头:负的绝对位置没有意义。
        MediaCommand::SeekBy(by) => {
            Some(position_ms.saturating_add(by).max(0))
        }
        _ => None,
    }
}

/// 绝对位置换成 Slint 的 `seek` 要的比例(见 `app.slint:74`)。
///
/// 时长为 0 —— 还没装起来,或上游没给 —— 时这次跳转没有意义,该被丢掉而不是除零。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn seek_ratio(
    target_ms: i64,
    duration_ms: i64,
) -> Option<f32> {
    if duration_ms <= 0 {
        return None;
    }

    let ratio = target_ms as f64 / duration_ms as f64;
    Some(ratio.clamp(0.0, 1.0) as f32)
}

/// 接上系统媒体控件:把两根线捏好交给平台入口,换回它那一端的实现。
///
/// 位置直接问播放器 —— 它是 `Send + Sync`(同播的事件本就在后台线程上用它),
/// 后端在自己的线程上拉不必绕回 UI 线程。命令反过来必须绕回来:Slint 的回调
/// 只能在事件循环上调。
///
/// 时长单独拿一个原子:跳转要靠它把绝对位置换成比例,而那道换算发生在后端
/// 送来的命令里,跨线程。它在这里造好,一份进闭包、一份进 [`Bridge`]。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn bind(
    ui: &MainWindow,
    player: &Arc<Result<audio::Player, audio::AudioError>>,
    media: impl FnOnce(MediaHooks) -> Box<dyn MediaControls>,
) -> Bridge {
    let duration =
        Arc::new(std::sync::atomic::AtomicI64::new(0));

    let hooks = MediaHooks {
        command: {
            let weak = ui.as_weak();
            let player = player.clone();
            let duration = duration.clone();
            Arc::new(move |command| {
                let player = player.clone();
                let duration = duration.clone();
                // 失败只有一种成因:事件循环已经没了。那时界面也不在了。
                weak.upgrade_in_event_loop(move |ui| {
                    dispatch(
                        &ui, &player, &duration, command,
                    );
                })
                .ok();
            })
        },
        position: {
            let player = player.clone();
            Arc::new(move || {
                player
                    .as_ref()
                    .as_ref()
                    .map(audio::Player::position)
                    .unwrap_or_default()
            })
        },
    };

    Bridge::new(media(hooks), duration)
}

/// 系统媒体控件按下的键落到界面上。
///
/// **只调 `.slint` 的回调,不碰任何状态。** `music::bind_controls` 里已经有一整套规矩
/// (收听同播时先退出、放空了就重放当前曲),在这里重写一遍就会立刻长歪。
#[cfg(not(target_arch = "wasm32"))]
fn dispatch(
    ui: &MainWindow,
    player: &Arc<Result<audio::Player, audio::AudioError>>,
    duration: &std::sync::atomic::AtomicI64,
    command: MediaCommand,
) {
    match command {
        MediaCommand::Next => ui.invoke_next_track(),
        MediaCommand::Previous => ui.invoke_prev_track(),
        MediaCommand::Play
        | MediaCommand::Pause
        | MediaCommand::Toggle => {
            if toggles(command, ui.get_is_playing()) {
                ui.invoke_toggle_play();
            }
        }
        MediaCommand::SetShuffle(_) => {
            if flips_shuffle(command, ui.get_shuffle_on()) {
                ui.invoke_shuffle_toggled();
            }
        }
        MediaCommand::SetLoop(_) => {
            if let Some(want) = wants_loop(
                command,
                loop_from_index(ui.get_loop_mode()),
            ) {
                ui.invoke_loop_mode_set(loop_index(want));
            }
        }
        MediaCommand::SeekTo(_)
        | MediaCommand::SeekBy(_) => {
            let at = player
                .as_ref()
                .as_ref()
                .map(|player| {
                    player.position().as_millis() as i64
                })
                .unwrap_or_default();
            let Some(target) = seek_target(command, at)
            else {
                return;
            };
            let Some(ratio) = seek_ratio(
                target,
                duration.load(
                    std::sync::atomic::Ordering::Relaxed,
                ),
            ) else {
                return;
            };
            ui.invoke_seek(ratio);
        }
    }
}

/// 把此刻在放的东西报给系统媒体控件。
///
/// 收 `playback` 与 `media` 而不是整个 `music::Deck`:取封面那个 future 只攥着这两样,
/// 为了推一次而把整个 deck 拖进闭包不值当。重复推是免费的,`Bridge` 自己去重。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn push(
    ui: &MainWindow,
    playback: &RefCell<Playback>,
    media: &Bridge,
) {
    let state = playback.borrow().state().clone();
    media.publish(NowPlaying::render(
        &state,
        ui.get_is_playing(),
        ui.get_shuffle_on(),
        loop_from_index(ui.get_loop_mode()),
        media.art(),
    ));
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use helpers::*;

    /// 测试里用的一首歌。只有 id 变,别的字段跟着 id 走。
    mod helpers {
        use std::rc::Rc;
        use std::sync::Arc;

        use app_core::TrackDto;

        use crate::media::NowPlaying;
        use crate::viz::CoverPixels;

        pub fn track(id: &str) -> TrackDto {
            TrackDto {
                platform: "netease".into(),
                id: id.into(),
                title: format!("歌 {id}"),
                alias: None,
                artists: vec!["甲".into(), "乙".into()],
                cover: Some(format!(
                    "https://cdn/{id}.jpg"
                )),
                duration_ms: 240_000,
            }
        }

        /// 一张 1×1 的封面。内容无关紧要,有没有才是被测的东西。
        pub fn art() -> Arc<CoverPixels> {
            Arc::new(CoverPixels {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
            })
        }

        /// 数一数后端被推了几次,以及最后一次推的是什么。
        #[derive(Default)]
        pub struct Spy {
            pub pushes: std::cell::RefCell<Vec<NowPlaying>>,
        }

        impl crate::media::MediaControls for Rc<Spy> {
            fn publish(&self, now: &NowPlaying) {
                self.pushes.borrow_mut().push(now.clone());
            }
        }
    }

    use std::rc::Rc;
    use std::sync::Arc;

    use app_core::{LoopMode, PlaybackState};

    use crate::media::{
        Bridge, MediaCommand, MediaStatus, NowPlaying,
        seek_ratio, seek_target, toggles,
    };

    /// 随机开没开要一并报出去,不然外面那个开关永远是灭的。
    #[test]
    fn now_playing_carries_the_shuffle_flag() {
        let state = PlaybackState::Playing(track("a"));

        let on =
            NowPlaying::render(&state, true, true, LoopMode::Off, None);
        let stopped = NowPlaying::render(&PlaybackState::Idle,
            false,
            true,
            LoopMode::Off, Some(art()),
        );

        assert!(on.shuffle);
        // 停下来抹掉的是曲目,不是播放器的开关:MPRIS 的 `Shuffle` 挂在
        // Player 接口上,与这一刻装没装着歌无关。
        assert!(
            stopped.shuffle,
            "停下来不该把随机也一起抹掉"
        );
        assert_eq!(
            stopped.track_id, "",
            "曲目那半照旧要清干净"
        );
    }

    /// **只拨了随机也要重新推一次。**
    ///
    /// 指纹不认随机的话,去重会把这次变更整个吃掉 —— 歌没换、放没放也没变,
    /// 于是系统控件上那个开关一直停在旧样子。
    #[test]
    fn toggling_shuffle_pushes_again() {
        let spy = Rc::new(Spy::default());
        let bridge = Bridge::new(
            Box::new(spy.clone()),
            Arc::default(),
        );
        let state = PlaybackState::Playing(track("a"));

        bridge.publish(NowPlaying::render(&state, true, false, LoopMode::Off, None,
        ));
        bridge.publish(NowPlaying::render(&state, true, true, LoopMode::Off, None,
        ));

        let pushes = spy.pushes.borrow();
        assert_eq!(
            pushes.len(),
            2,
            "歌没换、放没放也没变,但随机换了 —— 去重不该吃掉它"
        );
        assert!(!pushes[0].shuffle);
        assert!(pushes[1].shuffle);
    }

    /// 外面给的是绝对值,界面只有一个切换回调 —— 一样就别去动它。
    ///
    /// 与 [`toggles`] 同一道翻译,理由也同一个:让后端自己去猜「现在是不是
    /// 随机」,两个后端迟早有一个猜错。
    #[test]
    fn set_shuffle_only_flips_when_it_differs() {
        use crate::media::flips_shuffle;

        assert!(flips_shuffle(
            MediaCommand::SetShuffle(true),
            false
        ));
        assert!(flips_shuffle(
            MediaCommand::SetShuffle(false),
            true
        ));
        assert!(!flips_shuffle(
            MediaCommand::SetShuffle(true),
            true
        ));
        assert!(!flips_shuffle(
            MediaCommand::SetShuffle(false),
            false
        ));
        // 别的键跟随机无关,一个都不许翻
        assert!(!flips_shuffle(MediaCommand::Next, false));
        assert!(!flips_shuffle(MediaCommand::Toggle, true));
    }

    /// **只拨了循环也要重新推一次。**
    ///
    /// 与随机同一条:指纹不认它,去重会把这次变更吃掉,锁屏上的循环键
    /// 就一直停在旧样子。
    #[test]
    fn changing_the_loop_mode_pushes_again() {
        let spy = Rc::new(Spy::default());
        let bridge = Bridge::new(
            Box::new(spy.clone()),
            Arc::default(),
        );
        let state = PlaybackState::Playing(track("a"));

        bridge.publish(NowPlaying::render(&state, true, false, LoopMode::Off, None,
        ));
        bridge.publish(NowPlaying::render(&state, true, false, LoopMode::All, None,
        ));

        let pushes = spy.pushes.borrow();
        assert_eq!(
            pushes.len(),
            2,
            "歌没换、放没放也没变,但循环换了 —— 去重不该吃掉它"
        );
        assert_eq!(pushes[1].loop_mode, LoopMode::All);
    }

    /// 外面给的循环是绝对值,与现值一样就不折腾 —— 与随机同一道翻译。
    #[test]
    fn set_loop_only_changes_when_it_differs() {
        use crate::media::wants_loop;

        assert_eq!(
            wants_loop(
                MediaCommand::SetLoop(LoopMode::All),
                LoopMode::Off
            ),
            Some(LoopMode::All)
        );
        assert_eq!(
            wants_loop(
                MediaCommand::SetLoop(LoopMode::Off),
                LoopMode::Off
            ),
            None,
            "值本来就一样,按下它的人什么都没要求"
        );
        // 别的键跟循环无关。
        assert_eq!(
            wants_loop(MediaCommand::Next, LoopMode::Off),
            None
        );
    }

    /// 装着一首歌但没在走 = 暂停。
    ///
    /// `PlaybackState` 里没有「暂停」这个态,它记的是装载了哪一首;传输走没走
    /// 是另一件事。少了这一问,暂停之后系统控件上仍是播放中。
    #[test]
    fn a_loaded_but_paused_track_reports_paused() {
        let state = PlaybackState::Playing(track("a"));

        let running =
            NowPlaying::render(&state, true, false, LoopMode::Off, None);
        let paused =
            NowPlaying::render(&state, false, false, LoopMode::Off, None);

        assert_eq!(running.status, MediaStatus::Playing);
        assert_eq!(paused.status, MediaStatus::Paused);
        // 暂停的仍是那一首,曲目信息不该跟着状态一起没了。
        assert_eq!(paused.track_id, "a");
    }

    /// 空闲与失败都报 Stopped。
    ///
    /// 失败不是一种播放状态 —— 报成 Paused 会让外面显示一个「按一下就能继续」
    /// 的假象,而那首歌根本没装起来。
    #[test]
    fn an_idle_or_failed_deck_reports_stopped() {
        let idle = NowPlaying::render(&PlaybackState::Idle,
            false,
            false,
            LoopMode::Off, None,
        );
        let failed = NowPlaying::render(
            &PlaybackState::Failed("上游超时".into()),
            // 失败那一刻界面可能还没来得及把 is-playing 抹掉。
            true,
            false,
            LoopMode::Off,
            None,
        );

        assert_eq!(idle.status, MediaStatus::Stopped);
        assert_eq!(failed.status, MediaStatus::Stopped);
    }

    /// 停下来时不留着上一首。
    ///
    /// 队列放完了,控件上却还挂着最后那首歌的名字和封面 —— 这是「投影过期」
    /// 的老毛病换了个地方犯(见 `crate::notice` 的模块注释)。
    #[test]
    fn a_stopped_deck_carries_no_track() {
        let stopped = NowPlaying::render(&PlaybackState::Idle,
            false,
            false,
            LoopMode::Off, // 上一首的封面还攥在手里,也不该被带出去。
            Some(art()),
        );

        assert_eq!(stopped.track_id, "");
        assert_eq!(stopped.title, "");
        assert!(stopped.artists.is_empty());
        assert_eq!(stopped.duration_ms, 0);
        assert!(stopped.art_url.is_none());
        assert!(stopped.art.is_none());
    }

    /// 同一份快照不推第二次。
    ///
    /// 推送搭的是 1Hz 的续播轮询。不去重的话,一首四分钟的歌会往外发两百多次
    /// 状态变更,而内容一个字都没变。
    #[test]
    fn the_same_snapshot_is_not_published_twice() {
        let spy = Rc::new(Spy::default());
        let bridge = Bridge::new(
            Box::new(spy.clone()),
            Arc::default(),
        );
        let state = PlaybackState::Playing(track("a"));

        for _ in 0..5 {
            bridge.publish(NowPlaying::render(&state, true, false, LoopMode::Off, None,
            ));
        }

        assert_eq!(spy.pushes.borrow().len(), 1);
    }

    /// 封面晚到了要再推一次。
    ///
    /// 换歌那一刻封面还在路上(`play_current` 里那个 `spawn_local`),推出去的第一
    /// 份必然没有图。若指纹只认歌的 id,图到了也推不出去,控件上就永远是空封面。
    #[test]
    fn a_late_cover_is_published_again() {
        let spy = Rc::new(Spy::default());
        let bridge = Bridge::new(
            Box::new(spy.clone()),
            Arc::default(),
        );
        let state = PlaybackState::Playing(track("a"));

        bridge.publish(NowPlaying::render(&state, true, false, LoopMode::Off, None,
        ));
        bridge.publish(NowPlaying::render(&state,
            true,
            false,
            LoopMode::Off, Some(art()),
        ));

        let pushes = spy.pushes.borrow();
        assert_eq!(pushes.len(), 2);
        assert!(pushes[0].art.is_none());
        assert!(pushes[1].art.is_some());
    }

    /// 绝对跳转换成比例。
    ///
    /// 外面给的是绝对毫秒,Slint 的 `seek` 收的是 0..1(见 `app.slint:74`)。
    #[test]
    fn a_seek_target_becomes_a_ratio() {
        assert_eq!(seek_ratio(60_000, 240_000), Some(0.25));
        // 越界的目标夹住而不是溢出:外面拖到条尾时给的值可能略超时长。
        assert_eq!(seek_ratio(999_000, 240_000), Some(1.0));
        assert_eq!(seek_ratio(-5, 240_000), Some(0.0));
    }

    /// 没有时长就不跳。
    ///
    /// 还没装起来、或上游没给时长时,这次跳转没有意义 —— 该被丢掉,而不是除零。
    #[test]
    fn a_seek_without_a_duration_is_dropped() {
        assert_eq!(seek_ratio(60_000, 0), None);
        assert_eq!(seek_ratio(60_000, -1), None);
    }

    /// 相对跳转从当前位置起算。
    ///
    /// MPRIS 的 `Seek` 是相对的(快进 10 秒),安卓的 `onSeekTo` 是绝对的。
    /// 两者都在这里收敛成绝对位置,后端不必自己拿位置去加。
    #[test]
    fn a_relative_seek_starts_from_the_current_position() {
        let at = 30_000;

        assert_eq!(
            seek_target(MediaCommand::SeekTo(90_000), at),
            Some(90_000)
        );
        assert_eq!(
            seek_target(MediaCommand::SeekBy(10_000), at),
            Some(40_000)
        );
        // 往回跳过了头就落到开头,负的绝对位置没有意义。
        assert_eq!(
            seek_target(MediaCommand::SeekBy(-90_000), at),
            Some(0)
        );
        // 不是跳转的键在这里没有答案。
        assert_eq!(
            seek_target(MediaCommand::Next, at),
            None
        );
    }

    /// 已经在放时再按播放,什么都不该发生。
    ///
    /// 界面只有一个「切换」回调,而外面给的是 `Play`/`Pause`/`PlayPause` 三个键。
    /// 把 `Play` 一律当成切换,正在放的歌会被按停 —— 锁屏上最容易误触的就是它。
    #[test]
    fn play_while_already_playing_changes_nothing() {
        assert!(!toggles(MediaCommand::Play, true));
        assert!(toggles(MediaCommand::Play, false));

        assert!(toggles(MediaCommand::Pause, true));
        assert!(!toggles(MediaCommand::Pause, false));

        // 切换键名副其实,两种情形下都翻。
        assert!(toggles(MediaCommand::Toggle, true));
        assert!(toggles(MediaCommand::Toggle, false));
    }
}
