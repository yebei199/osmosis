//! 音乐页:搜歌、点一首出声。
//!
//! 本模块是**组装点**的一部分:它把 `api` 的请求函数和 `audio` 的播放器接到
//! `app_core::Playback` 上,三者互不相识。`app-core` 只知道"正在准备一首歌",
//! 不知道准备是靠 HTTP 还是靠 alsa。
//!
//! 显示层面的决定都落在这里,而不是服务端:歌手用什么符号拼、时长写成什么样,
//! 换个界面就该换个写法,不该固化进线上格式。

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{Playback, PlaybackState, TrackDto};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::{MainWindow, TrackRow};

/// 多歌手之间的分隔符。
const ARTIST_SEPARATOR: &str = " / ";

/// 一秒有多少毫秒。
const MILLIS_PER_SECOND: i64 = 1_000;

/// 一分钟有多少秒。
const SECONDS_PER_MINUTE: i64 = 60;

/// 歌手列表拼成一行。
///
/// 服务端保持列表形态是对的 —— 用「/」还是「&」是显示问题,只有界面知道。
pub fn join_artists(artists: &[String]) -> String {
    artists.join(ARTIST_SEPARATOR)
}

/// 毫秒时长写成 `分:秒`。
///
/// 负数在真实数据里不会出现(上游是 protobuf 的 int64,平台给的是正数),
/// 但真出现了也不该显示成 `-1:-30` —— 一并压到 0。
pub fn format_duration(duration_ms: i64) -> String {
    let total_seconds =
        (duration_ms / MILLIS_PER_SECOND).max(0);
    let minutes = total_seconds / SECONDS_PER_MINUTE;
    let seconds = total_seconds % SECONDS_PER_MINUTE;
    format!("{minutes}:{seconds:02}")
}

/// 把播放状态翻译成一行人类可读的文案。
pub fn describe_playback(state: &PlaybackState) -> String {
    match state {
        PlaybackState::Idle => "点一首歌开始".to_owned(),
        PlaybackState::Loading(track) => {
            format!("加载中… {}", track.title)
        }
        PlaybackState::Playing(track) => {
            format!("正在播放 {}", track.title)
        }
        PlaybackState::Failed(message) => {
            format!("失败: {message}")
        }
    }
}

/// 把一首歌翻成列表里的一行。所有格式化都在这里做完,`.slint` 只负责摆。
fn to_row(track: &TrackDto) -> TrackRow {
    TrackRow {
        id: track.id.clone().into(),
        title: track.title.clone().into(),
        artists: join_artists(&track.artists).into(),
        duration: format_duration(track.duration_ms).into(),
    }
}

/// 把搜索与播放接到音乐页上。
///
/// 音频设备只开一次并常驻:每次播放都重开设备的话,alsa 上会听到明显的咔哒声,
/// 而且第二次开可能因设备被自己占着而失败。开不出来(无声卡)不是致命错误 ——
/// 界面照常能搜歌,点播放时才报错。
#[cfg(not(target_arch = "wasm32"))]
pub fn bind(ui: &MainWindow) {
    let playback =
        Rc::new(RefCell::new(Playback::default()));
    // 搜索结果的权威副本。Slint 的 model 只存格式化后的字符串,
    // 点击时要靠它把 id 换回完整的 TrackDto。
    let tracks: Rc<RefCell<Vec<TrackDto>>> =
        Rc::new(RefCell::new(Vec::new()));
    let player = Rc::new(audio::Player::new());

    bind_search(ui, &tracks);
    bind_list(ui, &tracks);
    bind_play(ui, &playback, &tracks, &player);

    ui.set_playback_text(
        describe_playback(&PlaybackState::Idle).into(),
    );
}

/// wasm 上没有原生音频栈(见 `Cargo.toml` 的条件依赖)。界面照常在,
/// 只是这一页不接任何行为 —— 「余端 graceful 缺省」,不写平台判断到 `.slint` 里。
#[cfg(target_arch = "wasm32")]
pub fn bind(ui: &MainWindow) {
    ui.set_playback_text("Web 端暂不支持播放".into());
}

/// 「Web 端暂不支持播放」里的中文也得在子集字体里 —— 但它只在 wasm 上出现,
/// [`playback_copy_only_uses_subset_glyphs`] 那条守不到。这个常量把它摆到
/// 原生也能看见的地方,好让同一个测试覆盖。
#[cfg(test)]
const WASM_NOTICE: &str = "Web 端暂不支持播放";

/// 搜索:关键词 → `GET /search` → 结果列表。
#[cfg(not(target_arch = "wasm32"))]
fn bind_search(
    ui: &MainWindow,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
) {
    let tracks = tracks.clone();
    let weak = ui.as_weak();

    ui.on_search(move |keyword| {
        let keyword = keyword.to_string();
        if keyword.trim().is_empty() {
            return;
        }

        let tracks = tracks.clone();
        let weak = weak.clone();
        slint::spawn_local(async move {
            let found = api::search(&keyword).await;
            let Some(ui) = weak.upgrade() else { return };
            match found {
                Ok(dto) => show(&ui, &tracks, dto.tracks),
                Err(error) => {
                    // 搜索失败复用播放状态那一行 —— 音乐页只有一处报错位,
                    // 再加一行"搜索状态"会让两行里总有一行是空的。
                    ui.set_playback_text(
                        format!("失败: {error}").into(),
                    );
                }
            }
        })
        .expect("event loop must be running");
    });
}

/// 今日推荐与我喜欢的音乐。两者只差调哪个请求函数,其余完全相同。
#[cfg(not(target_arch = "wasm32"))]
fn bind_list(
    ui: &MainWindow,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
) {
    let daily = tracks.clone();
    let weak = ui.as_weak();
    ui.on_daily(move || {
        fetch_into(&weak, &daily, async {
            api::daily().await.map(|dto| dto.tracks)
        });
    });

    let liked = tracks.clone();
    let weak = ui.as_weak();
    ui.on_liked(move || {
        fetch_into(&weak, &liked, async {
            api::liked().await.map(|dto| dto.tracks)
        });
    });
}

/// 跑一个返回曲目列表的请求,结果填进列表,失败填进状态行。
///
/// 收的是 `Vec<TrackDto>` 而非线上的信封类型:`ui` 按分层不直接依赖 `contract`,
/// 剥壳在调用处一句 `.map(|dto| dto.tracks)` 完成。
///
/// 三个入口(搜索/推荐/红心)填的是**同一个** `tracks` 列表 ——
/// 换一个来源就整批换掉,不合并:合并了就说不清列表里这首是哪来的。
#[cfg(not(target_arch = "wasm32"))]
fn fetch_into<Fut>(
    weak: &slint::Weak<MainWindow>,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
    request: Fut,
) where
    Fut: core::future::Future<
            Output = Result<Vec<TrackDto>, api::ApiError>,
        > + 'static,
{
    let tracks = tracks.clone();
    let weak = weak.clone();
    slint::spawn_local(async move {
        let found = request.await;
        let Some(ui) = weak.upgrade() else { return };
        match found {
            Ok(found) => show(&ui, &tracks, found),
            Err(error) => ui.set_playback_text(
                format!("失败: {error}").into(),
            ),
        }
    })
    .expect("event loop must be running");
}

/// 把一批曲目同时装进 Slint 的 model 和 Rust 侧的权威副本。
#[cfg(not(target_arch = "wasm32"))]
fn show(
    ui: &MainWindow,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
    found: Vec<TrackDto>,
) {
    let rows: Vec<TrackRow> =
        found.iter().map(to_row).collect();
    *tracks.borrow_mut() = found;
    ui.set_tracks(ModelRc::new(VecModel::from(rows)));
}

/// 播放:id → 取直链 → 开流 → 解码 → 出声。
#[cfg(not(target_arch = "wasm32"))]
fn bind_play(
    ui: &MainWindow,
    playback: &Rc<RefCell<Playback>>,
    tracks: &Rc<RefCell<Vec<TrackDto>>>,
    player: &Rc<Result<audio::Player, audio::AudioError>>,
) {
    let playback = playback.clone();
    let tracks = tracks.clone();
    let player = player.clone();
    let weak = ui.as_weak();

    ui.on_play(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        let id = id.to_string();
        let Some(track) = tracks
            .borrow()
            .iter()
            .find(|track| track.id == id)
            .cloned()
        else {
            return;
        };

        // spawn_local 的 future 要到下一轮事件循环才跑,而 Loading 要立刻显示。
        // 同 `bind_health`:直接推一次文案,不为此发明订阅机制。
        ui.set_playback_text(
            describe_playback(&PlaybackState::Loading(
                track.clone(),
            ))
            .into(),
        );

        let playback = playback.clone();
        let player = player.clone();
        let weak = ui.as_weak();
        slint::spawn_local(async move {
            app_core::play(&playback, track, |track| {
                start(player.clone(), track)
            })
            .await;

            if let Some(ui) = weak.upgrade() {
                ui.set_playback_text(
                    describe_playback(
                        playback.borrow().state(),
                    )
                    .into(),
                );
            }
        })
        .expect("event loop must be running");
    });
}

/// 真正把一首歌变成声音:取直链 → 开流 → 解码 → 送进播放器。
///
/// 这就是注入给 `app_core::play` 的那个闭包体。`app-core` 只看到
/// "一个返回 Result 的 future",看不到 HTTP 也看不到 alsa。
#[cfg(not(target_arch = "wasm32"))]
async fn start(
    player: Rc<Result<audio::Player, audio::AudioError>>,
    track: TrackDto,
) -> Result<(), String> {
    let source = api::play_source(&track.id)
        .await
        .map_err(|error| error.to_string())?;
    // 开流与解码都在 `audio` 自己的后台 runtime 上跑 —— 这里是 Slint 的 UI 线程,
    // 没有 tokio 反应堆,也不能被阻塞读占住。
    let decoded = audio::load(&source.url)
        .await
        .map_err(|error| error.to_string())?;

    match player.as_ref() {
        Ok(player) => {
            player.play(decoded);
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    fn track() -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: "1".to_owned(),
            title: "紅蓮華".to_owned(),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 234_000,
        }
    }

    /// 四个状态都要有人能读的文案。少一个,用户就会遇到一行空白 ——
    /// 那时候他分不清是没点中、还是放不了。
    #[test]
    fn describe_playback_covers_every_state() {
        for state in [
            PlaybackState::Idle,
            PlaybackState::Loading(track()),
            PlaybackState::Playing(track()),
            PlaybackState::Failed("直链已过期".to_owned()),
        ] {
            assert!(
                !describe_playback(&state).is_empty(),
                "{state:?} 没有文案"
            );
        }
    }

    /// 多个歌手用固定分隔符拼成一行。
    #[test]
    fn track_row_joins_artists_with_separator() {
        assert_eq!(
            join_artists(&[
                "LiSA".to_owned(),
                "梶浦由記".to_owned()
            ]),
            "LiSA / 梶浦由記"
        );
    }

    /// 234000ms 是 3 分 54 秒。
    #[test]
    fn track_row_formats_duration_as_minutes_seconds() {
        assert_eq!(format_duration(234_000), "3:54");
    }

    /// 秒数不足两位要补零 —— 不补的话 61 秒会写成 `1:1`,读起来像 1 分 10 秒。
    #[test]
    fn track_row_pads_seconds_below_ten() {
        assert_eq!(format_duration(61_000), "1:01");
    }

    /// 平台没给时长时上游填 0。要写成 `0:00`,不能是空白 ——
    /// 空白会让那一列忽有忽无,整个列表跟着抖。
    #[test]
    fn track_row_handles_zero_duration() {
        assert_eq!(format_duration(0), "0:00");
    }

    /// 子集字体必须覆盖 [`describe_playback`] 会吐出的每一个非 ASCII 字符。
    ///
    /// 与 `lib.rs` 的 `describe_only_uses_subset_glyphs` 同一个守卫,只是管另一段文案:
    /// 新增中文而忘了重跑 `just font-subset`,这里就红。
    ///
    /// **歌名本身不在此列** —— 它是平台给的任意 CJK,不可能预裁,
    /// 界面上走系统字体(见 `app.slint` 音乐页的注释)。所以下面只喂 ASCII 标题,
    /// 检查的是文案里的固定部分。
    ///
    /// 只在原生上跑:失败文案取自 `audio::AudioError`,而 `audio` 是 wasm 下
    /// 不存在的条件依赖(见 `Cargo.toml`)。
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn playback_copy_only_uses_subset_glyphs() {
        use api::ApiError;
        use audio::AudioError;

        const CJK_SUBSET: &[u8] =
            include_bytes!("../fonts/cjk-subset.ttf");

        let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
            .expect("子集字体应能被解析");

        let ascii_track = TrackDto {
            title: "Gurenge".to_owned(),
            ..track()
        };

        // 失败原因不是手抄的:直接取两个错误类型的真实 Display 输出。
        // 它们各自带中文前缀,改了措辞而没重裁字体,这里会红。
        let failures: Vec<String> = vec![
            AudioError::Device("no device".to_owned())
                .to_string(),
            AudioError::Stream("refused".to_owned())
                .to_string(),
            AudioError::Decode("unrecognized".to_owned())
                .to_string(),
            ApiError::Transport("timed out".to_owned())
                .to_string(),
            ApiError::Decode("expected value".to_owned())
                .to_string(),
            ApiError::VersionMismatch {
                expected: 1,
                actual: 2,
            }
            .to_string(),
        ];

        let mut states = vec![
            PlaybackState::Idle,
            PlaybackState::Loading(ascii_track.clone()),
            PlaybackState::Playing(ascii_track),
        ];
        states.extend(
            failures.into_iter().map(PlaybackState::Failed),
        );

        for state in states {
            for ch in describe_playback(&state).chars() {
                assert!(
                    face.glyph_index(ch).is_some(),
                    "子集字体缺字 {ch:?}(状态 {state:?})—— 重跑 `just font-subset`"
                );
            }
        }

        // wasm 分支的那句提示不经过 describe_playback,单独查一遍。
        for ch in WASM_NOTICE.chars() {
            assert!(
                face.glyph_index(ch).is_some(),
                "子集字体缺字 {ch:?}(wasm 提示)—— 重跑 `just font-subset`"
            );
        }
    }
}
