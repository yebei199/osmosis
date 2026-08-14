//! 与界面无关的纯计算:文案、时长格式化,以及「该不该推进 / 该不该报错」这类判据。
//!
//! 抽在这里是为了能单独测 —— 它们不碰 Slint,也不碰播放器。

use super::*;

/// 多歌手之间的分隔符。
pub(super) const ARTIST_SEPARATOR: &str = " / ";

/// 一秒有多少毫秒。
pub(super) const MILLIS_PER_SECOND: i64 = 1_000;

/// 一分钟有多少秒。
pub(super) const SECONDS_PER_MINUTE: i64 = 60;

/// 队列放完时状态行上的话。常量而非字面量:子集字体测试要遍历到它。
pub(super) const QUEUE_DONE: &str = "队列放完了";

/// 自动续播的轮询间隔。rodio 没有"放完了"的回调,只能定期看一眼。
#[cfg(not(target_arch = "wasm32"))]
pub(super) const ADVANCE_POLL: core::time::Duration =
    core::time::Duration::from_secs(1);

/// 当前这首放过多久之后才去备下一首。
///
/// 早了会和正在放的那首抢带宽,晚了等于没预取。十秒:那时起播的那阵下载高峰
/// 已经过去,而离用户可能按「下一首」还早(见 [`should_prefetch`])。
#[cfg(not(target_arch = "wasm32"))]
pub(super) const PREFETCH_AFTER: core::time::Duration =
    core::time::Duration::from_secs(10);

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

/// 开机静默自检的结论:健康就闭嘴,坏了才开口。
///
/// Server 页删掉之后,这是版本协商唯一的运行时入口 —— 客户端与服务端的
/// `PROTOCOL_VERSION` 对不上时,这里的一行话是用户能得到的全部解释。
pub fn describe_startup(
    result: &Result<(), api::ApiError>,
) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(error) => Some(format!("失败: {error}")),
    }
}

/// 该不该起预取:本机在放、已经放过一阵、手里没有备着的、队列还有下一首。
///
/// **不能一起播就预取**:那样两条下载会抢同一条链路,而正在放的那首经不起抢
/// (取直链的 CDN 本来就爱停摆,见 `docs/adr/0013`)。等当前这首站稳了再备,
/// 反正备的是"还有一整首歌的时间"之后才用得上的东西。
///
/// 听众那一条与 [`should_advance`] 同理:收听时切歌的决定权不在本机,
/// 备了也用不上,白占一条下载。
#[cfg(not(target_arch = "wasm32"))]
pub fn should_prefetch(
    state: &PlaybackState,
    position: core::time::Duration,
    listening: bool,
    already_have: bool,
    has_next: bool,
) -> bool {
    matches!(state, PlaybackState::Playing(_))
        && position >= PREFETCH_AFTER
        && !listening
        && !already_have
        && has_next
}

/// 该不该报断流:本机在放、声源空了、而且这条流留下了放弃的证据。
///
/// 与 [`should_advance`] 是同一刻的两个出口,互斥:声源空下来时,要么是放完了
/// 该切下一首,要么是断了该停下说话。四个输入一模一样,只多问一句"放弃过没有" ——
/// 那正是两者唯一的分野(见 `docs/adr/0013`)。
///
/// 听众那一条与 [`should_advance`] 同理:收听时本机没有自己的流,`gave_up`
/// 反映的是上一次本机播放留下的旧证据,不能拿它去掐别人推来的声音。
pub fn should_report_loss(
    state: &PlaybackState,
    drained: bool,
    listening: bool,
    gave_up: bool,
) -> bool {
    matches!(state, PlaybackState::Playing(_))
        && drained
        && !listening
        && gave_up
}

/// 断流横幅该说哪句话。
///
/// `server_reachable` 是那次 `/health` 探测的结论:`None` = 还没回来。
/// 先弹粗文案再改精确文案,是为了不让沉默时长受探测连累(见 `docs/adr/0013`)。
///
/// 探得通只说明**我们自己的**服务端还在,上游 CDN 挂了也会落到这一支 ——
/// 所以那句话指向用户能做的动作,不去断言是谁的锅。
pub fn describe_stream_loss(
    server_reachable: Option<bool>,
) -> &'static str {
    match server_reachable {
        None => "播放中断了",
        Some(false) => "没网了,检查一下网络再试",
        Some(true) => "播放地址失效了,重新点一下这首歌",
    }
}

/// 自动续播的判据:**只有**「本机在放 && 声源放空了 && 不是听众」才推进队列。
///
/// 听众那一条是硬约束:听众放的 `ChannelSource` 在没数据时给静音而非结束,
/// 正常情况下 `drained` 不会为真;但万一将来有人改了那个行为,这里也不许
/// 在收听时切歌 —— 那会把对面推来的声音捣掉。
pub fn should_advance(
    state: &PlaybackState,
    drained: bool,
    listening: bool,
) -> bool {
    matches!(state, PlaybackState::Playing(_))
        && drained
        && !listening
}

/// 这一下点击是不是多余的。
///
/// 判据只认 `Loading`,而且只认**同一首**:网络慢时点了看不出反应,用户就会
/// 连点,每一下都发一次下载、每条回来都从头出声。已经在放的那一首再点是
/// 「从头听」,不是多余(见 `CONTEXT.md`「队列」)。
pub fn is_redundant_tap(
    state: &PlaybackState,
    id: &str,
) -> bool {
    matches!(state, PlaybackState::Loading(track) if track.id == id)
}

/// 当日推荐该不该拉。`last` 是上次拉取的日期,`today` 是今天。
///
/// 相等就不拉,于是搜完歌切出去再回来,搜索结果不会被推荐冲掉 —— 三个入口
/// (搜索/推荐/红心)填的是同一个列表,拉一次就整批换掉。
/// 不相等一律拉,包括 `last` 比 `today` 还晚的情况:时钟被拨过就重拉一次,
/// 比推理"是不是该信这个日期"便宜。
pub fn daily_is_due<D: PartialEq>(
    last: Option<&D>,
    today: &D,
) -> bool {
    last != Some(today)
}

/// 把一批歌翻成列表的行,顺带标出正在加载的那一首。
///
/// 所有格式化都在这里做完,`.slint` 只负责摆。`loading` 给的是那一首的 id ——
/// 它不在这批里(点完歌又搜了别的)就一行都不标。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn to_rows(
    batch: &[TrackDto],
    loading: Option<&str>,
) -> Vec<TrackRow> {
    batch
        .iter()
        .map(|track| TrackRow {
            id: track.id.clone().into(),
            title: track.title.clone().into(),
            artists: join_artists(&track.artists).into(),
            duration: format_duration(track.duration_ms)
                .into(),
            loading: loading == Some(track.id.as_str()),
            // 红心状态由 push_rows 之后的 remark 填 —— 这里没有那个集合,
            // 而把它传进来会让这个纯格式化函数多认识一样东西。
            liked: false,
            // 平台没给封面就是空串,那一行永远画占位色(见 tracklist.slint)。
            cover_url: track
                .cover
                .clone()
                .unwrap_or_default()
                .into(),
            // 图由 thumbnail 在行滑进可见区之后回填,与红心同理。
            cover: slint::Image::default(),
        })
        .collect()
}

/// 正在加载的那一首的 id,没有就给 `None`。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn loading_id(
    state: &PlaybackState,
) -> Option<&str> {
    match state {
        PlaybackState::Loading(track) => {
            Some(track.id.as_str())
        }
        _ => None,
    }
}

/// 当前这一首的 id —— 正在加载的和已经在放的都算,没有就给 `None`。
///
/// 与 [`loading_id`] 的差别正是"算不算已经放起来的那一首":那一个用来在列表上
/// 标加载态,这一个用来判断异步回来的东西**还是不是给当前这首的**。
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn current_id(
    state: &PlaybackState,
) -> Option<&str> {
    match state {
        PlaybackState::Loading(track)
        | PlaybackState::Playing(track) => {
            Some(track.id.as_str())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
