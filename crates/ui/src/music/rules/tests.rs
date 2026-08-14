use similar_asserts::assert_eq;

use super::super::fixtures::*;
use super::*;

/// 正在加载的就是这一首:再点它是多余的。
///
/// 用户会连点,正是因为网络慢时点了看不出反应 —— 每一下都发一次
/// 下载、每条回来都从头出声,这是 bug 不是"手快"。
#[test]
fn tapping_the_track_that_is_already_loading_is_redundant()
{
    assert!(is_redundant_tap(
        &PlaybackState::Loading(track_with_id("1")),
        "1"
    ));
}

/// 正在加载别的歌:这一下该换歌,不算多余。
#[test]
fn tapping_a_different_track_while_loading_is_not_redundant()
 {
    assert!(!is_redundant_tap(
        &PlaybackState::Loading(track_with_id("1")),
        "2"
    ));
}

/// 已经在放这一首:再点是「从头听」,照旧生效。
#[test]
fn tapping_the_playing_track_is_not_redundant() {
    assert!(!is_redundant_tap(
        &PlaybackState::Playing(track_with_id("1")),
        "1"
    ));
}

/// 空闲与失败态下的点击一律照常 —— 失败之后重试是常见动作。
#[test]
fn tapping_while_idle_or_failed_is_not_redundant() {
    assert!(!is_redundant_tap(&PlaybackState::Idle, "1"));
    assert!(!is_redundant_tap(
        &PlaybackState::Failed("直链已过期".to_owned()),
        "1"
    ));
}

/// 从没拉过:该拉。
#[test]
fn daily_has_never_been_fetched_so_it_is_due() {
    assert!(daily_is_due(None, &20_260_730));
}

/// 今天拉过了:不动 —— 搜索结果因此保得住。
#[test]
fn daily_fetched_today_is_not_due() {
    assert!(!daily_is_due(Some(&20_260_730), &20_260_730));
}

/// 拉的是昨天的:跨天失效,该重拉。
#[test]
fn daily_fetched_on_an_earlier_day_is_due() {
    assert!(daily_is_due(Some(&20_260_729), &20_260_730));
}

/// 只标出加载中的那一行,其余行不受影响。
#[test]
fn only_the_loading_track_row_is_marked() {
    let batch = [
        track_with_id("1"),
        track_with_id("2"),
        track_with_id("3"),
    ];
    let rows = to_rows(&batch, Some("2"));
    assert_eq!(
        rows.iter()
            .map(|row| row.loading)
            .collect::<Vec<_>>(),
        vec![false, true, false]
    );
}

/// 没有歌在加载时,一行都不标。
#[test]
fn no_row_is_marked_when_nothing_is_loading() {
    let batch = [track_with_id("1"), track_with_id("2")];
    let rows = to_rows(&batch, None);
    assert!(rows.iter().all(|row| !row.loading));
}

/// 加载中的歌不在当前列表里(点完歌又搜了别的):一行不标,也不出错。
#[test]
fn a_loading_track_outside_the_list_marks_nothing() {
    let batch = [track_with_id("1"), track_with_id("2")];
    let rows = to_rows(&batch, Some("99"));
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.loading));
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

/// 自动续播只在「本机在放 && 放空了 && 不是听众」时才动手。
///
/// 第二行是本条真正要守的:**收听同播时绝不切歌**,即使声源看起来空了 ——
/// 切了就是把对面推来的声音捣掉。
#[test]
fn advances_only_when_playing_and_drained_and_not_listening()
 {
    let playing = PlaybackState::Playing(track());

    assert!(should_advance(&playing, true, false));
    assert!(
        !should_advance(&playing, true, true),
        "收听中不许自动切歌"
    );
    assert!(!should_advance(&playing, false, false));
    assert!(!should_advance(
        &PlaybackState::Idle,
        true,
        false
    ));
    assert!(!should_advance(
        &PlaybackState::Loading(track()),
        true,
        false
    ));
}

/// **断流与放完了要走向相反的出口。**
///
/// 两者在播放器那头是同一个现象(声源空了),区别只有那条流留没留下放弃的
/// 证据。分不清的话,断网时会一首接一首地切下去,每首再熬一轮超时,
/// 一分钟就把整个队列烧光,而用户得到的解释是零(见 `docs/adr/0013`)。
#[test]
fn a_drained_source_reports_a_loss_only_when_it_gave_up() {
    let playing = PlaybackState::Playing(track());

    assert!(
        should_report_loss(&playing, true, false, true),
        "放空了且流放弃过,这就是断流"
    );
    assert!(
        !should_report_loss(&playing, true, false, false),
        "放空了但流没放弃,那是正常放完,该切下一首"
    );
    // 两个出口必须互斥,否则同一刻既报错又切歌。
    assert!(
        !should_advance(&playing, true, false)
            || !should_report_loss(
                &playing, true, false, false
            )
    );
    assert!(
        !should_report_loss(&playing, false, false, true),
        "还没放空就报断流,声音还在放呢"
    );
    assert!(
        !should_report_loss(&playing, true, true, true),
        "收听同播时本机没有自己的流,那是上一次留下的旧证据"
    );
    assert!(
        !should_report_loss(
            &PlaybackState::Loading(track()),
            true,
            false,
            true
        ),
        "正在加载下一首时不许被上一首的证据打断"
    );
}

/// **预取要等当前这首站稳了再起。**
///
/// 一起播就备的话,两条下载抢同一条链路,而正在放的那首经不起抢 ——
/// 这个 CDN 本来就爱停摆(真机日志里连续四次失联,见 `docs/adr/0013`)。
#[test]
fn prefetch_starts_once_the_current_track_is_under_way() {
    let playing = PlaybackState::Playing(track());
    let under_way = PREFETCH_AFTER;
    let just_started = core::time::Duration::ZERO;

    assert!(should_prefetch(
        &playing, under_way, false, false, true
    ));
    assert!(
        !should_prefetch(
            &playing,
            just_started,
            false,
            false,
            true
        ),
        "刚起播就备下一首会和它自己抢带宽"
    );
    assert!(
        !should_prefetch(
            &playing, under_way, false, true, true
        ),
        "手里已经有备好的了,别再起一条"
    );
    assert!(
        !should_prefetch(
            &playing, under_way, false, false, false
        ),
        "队尾之后没有下一首可备"
    );
    assert!(
        !should_prefetch(
            &PlaybackState::Loading(track()),
            under_way,
            false,
            false,
            true
        ),
        "这一首自己还没放起来,轮不到备下一首"
    );
}

/// 收听同播时不预取:切歌的决定权不在本机,备了也用不上,白占一条下载。
#[test]
fn a_listener_never_prefetches() {
    assert!(!should_prefetch(
        &PlaybackState::Playing(track()),
        PREFETCH_AFTER,
        true,
        false,
        true
    ));
}

/// 横幅先说发生了什么,探明之后再说是哪一种。
///
/// 三句话必须互不相同:探测没回来时说"中断了"是诚实的,而一旦探明,
/// 用户该做的事完全不同 —— 一个是等网络,一个是重新点这首歌。
#[test]
fn the_banner_says_what_the_user_can_do_about_it() {
    let pending = describe_stream_loss(None);
    let offline = describe_stream_loss(Some(false));
    let stale = describe_stream_loss(Some(true));

    assert!(!pending.is_empty());
    assert_ne!(pending, offline);
    assert_ne!(offline, stale);
    assert_ne!(pending, stale);
}

/// 开机自检只在坏消息时开口:健康 → None,失败 → 一行能显示的话。
#[test]
fn startup_check_speaks_only_on_failure() {
    assert_eq!(describe_startup(&Ok(())), None);

    let mismatch = describe_startup(&Err(
        api::ApiError::VersionMismatch {
            expected: 1,
            actual: 2,
        },
    ))
    .expect("版本不匹配必须开口");
    assert!(
        mismatch.contains("协议版本不匹配"),
        "文案要点名版本问题,实得 {mismatch}"
    );

    let transport =
        describe_startup(&Err(api::ApiError::Transport(
            "connection refused".to_owned(),
        )))
        .expect("连不上必须开口");
    assert!(
        transport.contains("网络错误"),
        "文案要点名网络问题,实得 {transport}"
    );
}

/// 子集字体必须覆盖本模块会吐出的每一个非 ASCII 字符。
///
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
        include_bytes!("../../../fonts/cjk-subset.ttf");

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

    let mut copy: Vec<String> =
        states.iter().map(describe_playback).collect();
    // 不经过 describe_playback 的固定文案,单独列上。
    copy.push(QUEUE_DONE.to_owned());
    copy.push(WASM_NOTICE.to_owned());
    copy.push("同播: 没有其他设备".to_owned());
    // 听众收听时的播放行(见 `syncplay.rs` 的 Listening 分支)。
    copy.push("收听中…".to_owned());
    // 开机自检的两种坏消息。
    copy.extend(
        [
            describe_startup(&Err(
                api::ApiError::VersionMismatch {
                    expected: 1,
                    actual: 2,
                },
            )),
            describe_startup(&Err(
                api::ApiError::Transport(
                    "refused".to_owned(),
                ),
            )),
        ]
        .into_iter()
        .flatten(),
    );

    for line in copy {
        for ch in line.chars() {
            assert!(
                face.glyph_index(ch).is_some(),
                "子集字体缺字 {ch:?}(文案 {line:?})—— 重跑 `just font-subset`"
            );
        }
    }
}
