//! bang-dream 聚合层的 gRPC 客户端,以及它的领域模型到 [`contract`] 的翻译。
//!
//! 这里是本服务唯一认识 gRPC 的地方 —— 往上只有 [`contract`] 里的 DTO。
//! 客户端因此不必知道 bang-dream 的存在,也不必为上游 proto 的演化重新编译。
//!
//! 翻译刻意**裁剪**:上游的 `Track` 有音质规格、付费等级等等,这里只留客户端
//! 此刻用得上的字段。加字段是兼容变更,用到时再加。

use std::collections::HashSet;

use contract::{
    ArtistDto, LyricDto, LyricLineDto, PlaySourceDto,
    PlaylistDto, PlaylistSource, TrackDto,
};

use crate::account::Account;
use crate::cache::TrackRef;

/// 由 `build.rs` 从 `third_party/bang-dream/proto` 生成。
pub mod proto {
    tonic::include_proto!("bangdream.music.v1");
}

/// bang-dream 认这个 metadata 键来选该用哪个账号的网易云凭据。
///
/// 键名与那侧的 `internal/rpc/userid.go` 手工对齐 —— proto 里没有它,
/// 因为「以谁的身份问」不是领域请求的一部分(见那个仓库的 `docs/adr/0009`)。
const USER_ID_KEY: &str = "x-user-id";

/// 把一条请求包成带用户标识的 gRPC 请求。
///
/// 上游没有默认用户:不带这个头的调用一律 `INVALID_ARGUMENT`。所以每一次上游调用
/// 都要经过这里 —— 漏一处的现象是那条路由整个不可用,不会静默串号。
pub fn as_user<T>(
    account: &Account,
    message: T,
) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    // 用户标识是 accounts.id 的十进制串,永远是合法的 ASCII metadata 值
    let value = account.upstream_user_id().parse().expect(
        "用户标识是十进制数字,必然是合法的 metadata 值",
    );

    request.metadata_mut().insert(USER_ID_KEY, value);

    request
}

/// 上游平台枚举翻成契约里的字符串。
///
/// 用字符串而非数字:契约要能被人读懂,也要在加平台时不依赖枚举序号的稳定性。
///
/// 缓存也按这个值存(见 `cache.rs`)—— 另写一份的话,prost 生成的
/// `as_str_name()` 给的是 `PLATFORM_NETEASE`,与这里的 `netease` 对不上,
/// 而那是运行期才炸的外键错误,编译器一声不吭。
pub fn platform_name(raw: i32) -> String {
    match proto::Platform::try_from(raw) {
        Ok(proto::Platform::Netease) => "netease",
        _ => "unknown",
    }
    .to_owned()
}

/// 空串翻成 `None`。
///
/// protobuf 的 `string` 没有"缺席"这个状态,平台没给就是空串。契约区分二者:
/// `None` 是"平台没给",`Some("")` 会被客户端当成一个真实存在的空值。
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

/// 把上游的一个歌手翻成契约里的 [`ArtistDto`]。
///
/// 只在**搜歌手**这条路上用。内嵌在 `Track` 里的歌手走另一条路,翻成一串名字 ——
/// 那里要的是显示,这里要的是能点进去的实体。
pub fn artist_to_dto(artist: proto::Artist) -> ArtistDto {
    ArtistDto {
        id: artist.id,
        name: artist.name,
        avatar: non_empty(artist.avatar),
        album_count: artist.album_count,
    }
}

/// 把上游的一个歌单翻成契约里的 [`PlaylistDto`]。
///
/// `source` 一律是 `Platform`:能走到这个函数的都来自音乐平台。本地歌单
/// 由 [`crate::playlist`] 那侧翻,两条路各自打标,不共用一个带参数的函数 ——
/// 那样标错了不会有任何编译错误。
pub fn playlist_to_dto(
    list: proto::Playlist,
) -> PlaylistDto {
    PlaylistDto {
        source: PlaylistSource::Platform,
        id: list.id,
        name: list.name,
        cover: non_empty(list.cover),
        track_count: list.track_count,
    }
}

/// 网易云给红心歌单打的标记。
///
/// 别的特殊类型(年度歌单是 20)都是真歌单,所以判的是这一个值,不是「非零」。
///
/// 这个魔数是网易云的私有枚举,本该住在 bang-dream 里 —— `docs/adr/0022` 把它
/// 换到了这边,理由与代价都记在那篇。接第二个平台时这里会长出平台分支。
const LIKED_SPECIAL_TYPE: i32 = 5;

/// 把上游的歌单列表翻成平台那半张,顺带把红心歌单摘出去。
///
/// 摘它是因为「我喜欢的」在客户端是**另一个来源**([`PlaylistSource::Liked`]),
/// 由 [`crate::playlist::merged`] 单独置顶。不摘的话列表里会并排站着两个
/// 「我喜欢的」,而它们连 id 都不一样 —— 去重的活没人干得对。
pub fn platform_playlists_to_dto(
    lists: Vec<proto::Playlist>,
) -> Vec<PlaylistDto> {
    lists
        .into_iter()
        .filter(|list| {
            list.special_type != LIKED_SPECIAL_TYPE
        })
        .map(playlist_to_dto)
        .collect()
}

/// 从歌单列表里认出红心歌单的 id。
///
/// 与 [`platform_playlists_to_dto`] 是同一个判据的两面:那边把它摘出去,
/// 这边把它挑出来。判据只写一处([`LIKED_SPECIAL_TYPE`]),两边错开的话
/// 现象是红心既不在平台列表里、也取不到详情。
pub fn liked_playlist_id(
    lists: &[proto::Playlist],
) -> Option<String> {
    lists
        .iter()
        .find(|list| {
            list.special_type == LIKED_SPECIAL_TYPE
        })
        .map(|list| list.id.clone())
}

/// 上游的歌单详情少给了哪些曲目的详情。
///
/// 歌单详情一次会带回一批完整曲目,但那一批**会被平台截断**,而标识列表是全量的。
/// 够全时一次补拉都不必发;截断了就只补差额 —— 不是整份重取。
///
/// 返回的次序跟着 `refs` 走,便于调用方按批切片。
pub fn refs_missing_from(
    detail_tracks: &[TrackDto],
    refs: &[TrackRef],
) -> Vec<String> {
    let present: HashSet<&str> = detail_tracks
        .iter()
        .map(|track| track.id.as_str())
        .collect();

    refs.iter()
        .filter(|track| {
            !present.contains(track.id.as_str())
        })
        .map(|track| track.id.clone())
        .collect()
}

/// 剔掉平台给不出详情的那些成员关系,并报出剔了几条。
///
/// 成员关系必须先有详情,否则外键会拒绝(见 `0004` 那条注释)。所以拿不到详情的
/// 曲目只能不写进去 —— 但**必须报出来**:不报的话歌单会静默变短,而用户看到的
/// 只是数目对不上,分不清「我少点了一个红心」和「平台不给这首歌的详情」。
///
/// 次序跟着 `refs` 走,过滤不重排。
pub fn keep_available(
    refs: &[TrackRef],
    unavailable: &HashSet<String>,
) -> (Vec<TrackRef>, usize) {
    let known: Vec<TrackRef> = refs
        .iter()
        .filter(|track| !unavailable.contains(&track.id))
        .cloned()
        .collect();

    // 用差值而不是 `unavailable.len()`:那个集合里可能有不属于这个歌单的 id,
    // 拿它当条数会报出一个用户在这张列表上永远对不上的数字。
    let dropped = refs.len() - known.len();

    (known, dropped)
}

/// 把上游的一首歌翻成契约里的 [`TrackDto`]。
pub fn track_to_dto(track: proto::Track) -> TrackDto {
    TrackDto {
        platform: platform_name(track.platform),
        id: track.id,
        title: track.title,
        alias: non_empty(track.alias),
        artists: track
            .artists
            .into_iter()
            .map(|artist| artist.name)
            .collect(),
        cover: non_empty(track.cover),
        duration_ms: track.duration_ms,
    }
}

/// 把上游的一次播放源翻成契约里的 [`PlaySourceDto`]。
///
/// 不带 `size` 与 `level`:客户端边下边播,不需要预先知道体积;
/// 实际档位目前也没有消费者。用到时再加。
pub fn play_source_to_dto(
    source: proto::PlaySource,
) -> PlaySourceDto {
    PlaySourceDto {
        url: source.url,
        format: source.format,
        bit_rate: source.bit_rate,
        trial: source.trial,
    }
}

/// 把上游的歌词翻成契约里的 [`LyricDto`]。
///
/// 只取行级时间轴:逐字档位下上游已把整行 `text` 拼好(见 proto 的 `LyricLine`
/// 注释),行级消费方因此不必关心上游给的是哪一档。罗马音暂无消费者,不带。
///
/// 翻完过一道 [`split_long_lines`]:平台给的行粒度不可控,超长行要在这里切开。
pub fn lyric_to_dto(lyric: proto::Lyric) -> LyricDto {
    LyricDto {
        lines: split_long_lines(
            lyric
                .lines
                .into_iter()
                .map(|line| LyricLineDto {
                    start_ms: line.start_ms,
                    end_ms: line.end_ms,
                    text: line.text,
                    translation: non_empty(
                        line.translation,
                    ),
                })
                .collect(),
        ),
    }
}

/// 超过多少个字符才算「超长」,需要二次切分。
///
/// 这是个粗糙代理量:真正决定折几行的是渲染宽度与字号,而本层不知道客户端是
/// 宽版式还是 compact,更不知道中文字符比拉丁字符宽。取 100 是照实测卡的 ——
/// issue #21 里 109 字符折三行、控制条干净,198 字符折六行才撑破布局。
/// ponytail: 字符数够用就先用字符数;真要精确得让客户端把可用宽度报上来。
const MAX_LINE_CHARS: usize = 100;

/// 可以断行的标点。断在它**之后**,标点跟着上一段走。
///
/// 只收句读级的,不收引号与括号 —— 在 `(` 之后断开会留下一个孤儿括号。
const BREAKS: [char; 12] = [
    ',', '.', ';', '!', '?', '，', '。', '；', '！', '？',
    '、', '…',
];

/// 超长行按标点二次切分,时刻线性摊到切出的各段上。
///
/// 为什么在这一层做:平台给的行粒度不可控,一行 198 字符的整段词推到播放页
/// 会撑破布局(见 issue #21)。切分是对上游脏数据的归一,和 `non_empty` 同类,
/// 不是画法 —— 所有客户端都该拿到同样粒度的行。
///
/// 取整表而非逐行:上游的 `end_ms` 常常缺席,那时唯一的终点线索是**下一行**的
/// 起点,单看一行插不出时间。
fn split_long_lines(
    lines: Vec<LyricLineDto>,
) -> Vec<LyricLineDto> {
    let ends: Vec<Option<i64>> = (0..lines.len())
        .map(|index| effective_end(&lines, index))
        .collect();

    lines
        .into_iter()
        .zip(ends)
        .flat_map(|(line, end)| split_one(line, end))
        .collect()
}

/// 某一行的有效终点。`None` 表示无从插值,该行不切。
///
/// `end_ms` 缺席时上游给的是 `start_ms` 本身(见契约里 `LyricLineDto` 的注释),
/// 故判据是"严格大于"而不是"非零"。
fn effective_end(
    lines: &[LyricLineDto],
    index: usize,
) -> Option<i64> {
    let start = lines[index].start_ms;
    if lines[index].end_ms > start {
        return Some(lines[index].end_ms);
    }
    lines
        .get(index + 1)
        .map(|next| next.start_ms)
        .filter(|next| *next > start)
}

/// 切一行。三种情况原样返回:无从插值、本就不长、没有标点可断。
///
/// 没有标点时**不**硬切:按字数切会把单词劈成两半,比省略号更难读。
fn split_one(
    line: LyricLineDto,
    end: Option<i64>,
) -> Vec<LyricLineDto> {
    let Some(end) = end else { return vec![line] };
    if line.text.chars().count() <= MAX_LINE_CHARS {
        return vec![line];
    }

    let chunks = pack(break_at_punctuation(&line.text));
    if chunks.len() < 2 {
        return vec![line];
    }

    // 按字数线性摊时间。总数取切分后的字数之和 —— 断点处被 trim 掉的空格
    // 不占时长,用它做分母才让各段的起点落在原文的比例位置上。
    let weights: Vec<usize> =
        chunks.iter().map(|c| c.chars().count()).collect();
    let total: usize = weights.iter().sum();
    let span = end - line.start_ms;
    let mut consumed = 0usize;
    let translations =
        split_translation(line.translation, &weights);

    chunks
        .into_iter()
        .zip(translations)
        .map(|(text, translation)| {
            let start = line.start_ms
                + span * consumed as i64 / total as i64;
            consumed += text.chars().count();
            LyricLineDto {
                start_ms: start,
                end_ms: line.start_ms
                    + span * consumed as i64 / total as i64,
                text,
                translation,
            }
        })
        .collect()
}

/// 把整行的译文摊到正文切出的各段上,`weights` 是各段的字数。
///
/// 为什么不让各段共用整份译文:译文槽是定高的(`viz-lyric-tr-h`)且垂直居中,
/// 放不下时 Slint 裁的是中间那一截 —— 开头没了、结尾省略号,读起来是断头话。
/// 实拍见 issue #21。摊开之后每段只剩自己那一截,自然放得下。
///
/// 对不齐是必然的:中英的分句数不一样,没有逐句对应关系可用。所以按**正文的字数
/// 比例**分配 —— 正文走到三分之一处,译文也切在三分之一附近。这是近似,不是对译。
fn split_translation(
    translation: Option<String>,
    weights: &[usize],
) -> Vec<Option<String>> {
    let Some(translation) = translation else {
        return vec![None; weights.len()];
    };

    let pieces = break_at_punctuation(&translation);
    // 没有标点可断:整份给第一段。与其在每段里重复同一句读不全的话,
    // 不如让它只在它对应的那一段出现。
    if pieces.len() < 2 {
        return std::iter::once(Some(translation))
            .chain(std::iter::repeat_n(
                None,
                weights.len() - 1,
            ))
            .collect();
    }

    let total_weight: usize = weights.iter().sum();
    let total_chars = translation.chars().count();
    // 各段在译文里的终点(字符数)。用累计权重换算,避免逐段取整累积误差。
    let bounds: Vec<usize> = weights
        .iter()
        .scan(0usize, |cumulative, weight| {
            *cumulative += weight;
            Some(total_chars * *cumulative / total_weight)
        })
        .collect();

    let mut groups = vec![String::new(); weights.len()];
    let mut consumed = 0usize;
    for piece in pieces {
        let length = piece.chars().count();
        // 按片的**中点**归属,而不是起点或终点 —— 跨界的片归给它落得更多的那一段。
        let midpoint = consumed + length / 2;
        let index = bounds
            .iter()
            .position(|bound| midpoint < *bound)
            .unwrap_or(weights.len() - 1);
        groups[index].push_str(&piece);
        consumed += length;
    }

    groups
        .into_iter()
        .map(|group| non_empty(group.trim().to_owned()))
        .collect()
}

/// 在标点之后断开。段内**保留原样**(含前导空格),拼起来必须等于原文 ——
/// 中文没有词间空格,靠拼接时补空格会凭空塞出分隔符。
fn break_at_punctuation(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if BREAKS.contains(&ch) {
            pieces.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

/// 贪心合并相邻片段,直到再加一片就超阈值。
///
/// 贪心而非等分:等分会把"就差两个字"的一片单独甩成一段,读起来更碎。
fn pack(pieces: Vec<String>) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    for piece in pieces {
        match chunks.last_mut() {
            Some(last)
                if last.chars().count()
                    + piece.chars().count()
                    <= MAX_LINE_CHARS =>
            {
                last.push_str(&piece);
            }
            _ => chunks.push(piece),
        }
    }
    chunks
        .into_iter()
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 造一行契约歌词。切分只看时刻与文本,译文单独在对应用例里给。
    fn dto_line(
        start_ms: i64,
        end_ms: i64,
        text: &str,
    ) -> LyricLineDto {
        LyricLineDto {
            start_ms,
            end_ms,
            text: text.to_owned(),
            translation: None,
        }
    }

    /// 一行 108 字符的英文,带足够多的逗号可切。
    const LONG: &str = "So I do, I still feel the same, guess I play this guitar, hoping that tomorrow I can say, I'm doing fine";

    /// 短行原样通过,一个字段都不动。
    #[test]
    fn short_lines_pass_through_unchanged() {
        let lines = vec![
            dto_line(0, 1_000, "短短一行"),
            dto_line(1_000, 2_000, "another short one"),
        ];
        assert_eq!(split_long_lines(lines.clone()), lines);
    }

    /// 超长行在标点处断开,切出的每段都不再超阈值。
    #[test]
    fn long_line_splits_at_punctuation() {
        let out = split_long_lines(vec![dto_line(
            0, 4_000, LONG,
        )]);
        assert!(
            out.len() > 1,
            "108 字符的行应当被切开,实际 {} 段",
            out.len()
        );
        for line in &out {
            assert!(
                line.text.chars().count() <= MAX_LINE_CHARS,
                "切出的段仍超阈值:{:?}",
                line.text
            );
        }
        // 切分只断开,不吞字:各段接起来还是原文(容许断点处的空格被吃掉)。
        let rejoined: String = out
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(rejoined, LONG);
    }

    /// 切出的段按字数线性分配时刻:首段起点等于原行起点,末段不越过原行终点。
    #[test]
    fn split_lines_share_the_original_time_span() {
        let out = split_long_lines(vec![dto_line(
            10_000, 14_000, LONG,
        )]);
        assert_eq!(out[0].start_ms, 10_000);
        assert!(
            out.last().unwrap().end_ms <= 14_000,
            "末段越过了原行终点"
        );
        // 时刻必须单调递增,否则 current_line 会选错行。
        for pair in out.windows(2) {
            assert!(
                pair[0].start_ms < pair[1].start_ms,
                "切出的段时刻没有递增:{:?}",
                out
            );
        }
    }

    /// 上游没给 end_ms(与 start_ms 相同)时,拿下一行的起点当终点插值。
    #[test]
    fn missing_end_ms_falls_back_to_next_line_start() {
        let out = split_long_lines(vec![
            dto_line(10_000, 10_000, LONG),
            dto_line(14_000, 14_000, "下一句"),
        ]);
        let split: Vec<_> = out
            .iter()
            .filter(|line| line.start_ms < 14_000)
            .collect();
        assert!(split.len() > 1, "有下一行兜底时应当切开");
        for line in &split {
            assert!(
                line.start_ms < 14_000,
                "切出的段越过了下一行的起点"
            );
        }
    }

    /// 末行且没有 end_ms:无从插值,不切,原样留给 UI 省略号。
    #[test]
    fn last_line_without_end_ms_is_left_alone() {
        let lines = vec![dto_line(10_000, 10_000, LONG)];
        assert_eq!(split_long_lines(lines.clone()), lines);
    }

    /// 超长但一个标点都没有:不硬切,原样保留 —— 硬切会把单词劈成两半。
    #[test]
    fn long_line_without_punctuation_is_left_alone() {
        let text = "a".repeat(MAX_LINE_CHARS + 50);
        let lines = vec![dto_line(0, 4_000, &text)];
        assert_eq!(split_long_lines(lines.clone()), lines);
    }

    /// [`LONG`] 的译文,标点够多、切得开。
    const LONG_TRANSLATION: &str = "所以我确实如此,我仍然感觉一样的感受,我猜我弹这把吉他,希望明天我能说我很好,好吧,好吧,太阳每天都升起";

    /// 造一行带译文的超长行。
    fn long_line_with(translation: &str) -> LyricLineDto {
        let mut line = dto_line(0, 4_000, LONG);
        line.translation = Some(translation.to_owned());
        line
    }

    /// 各段的译文按顺序拼起来。
    fn joined_translation(
        lines: &[LyricLineDto],
    ) -> String {
        lines
            .iter()
            .filter_map(|line| line.translation.as_deref())
            .collect()
    }

    /// 译文按自己的标点切片后分给各段,拼起来等于原译文,一个字不丢。
    #[test]
    fn the_segments_together_hold_the_whole_translation() {
        let out = split_long_lines(vec![long_line_with(
            LONG_TRANSLATION,
        )]);
        assert!(out.len() > 1);
        assert_eq!(
            joined_translation(&out),
            LONG_TRANSLATION
        );
    }

    /// 分配比例跟着正文走:正文最长的那一段,拿到的译文也最长。
    #[test]
    fn translation_shares_follow_the_text_proportions() {
        let out = split_long_lines(vec![long_line_with(
            LONG_TRANSLATION,
        )]);
        let widest = out
            .iter()
            .max_by_key(|line| line.text.chars().count())
            .expect("切分至少给出一段");
        let richest = out
            .iter()
            .max_by_key(|line| {
                line.translation
                    .as_deref()
                    .map_or(0, |t| t.chars().count())
            })
            .expect("切分至少给出一段");
        assert_eq!(widest.start_ms, richest.start_ms);
    }

    /// 译文片数少于正文段数时,多出来的段不带译文,而不是回头重复整段。
    ///
    /// 用加长版正文:[`LONG`] 只切得出两段,凑不出「片数少于段数」。
    #[test]
    fn fewer_translation_pieces_than_segments_leaves_the_rest_empty()
     {
        let mut line =
            dto_line(0, 4_000, &format!("{LONG}, {LONG}"));
        line.translation = Some("甲,乙".to_owned());
        let out = split_long_lines(vec![line]);
        assert!(out.len() > 2);
        let carried = out
            .iter()
            .filter(|line| line.translation.is_some())
            .count();
        assert_eq!(carried, 2);
    }

    /// 没有标点可断的译文整份给第一段,其余段留空 —— 与其在三段里重复一句
    /// 读不全的话,不如只在它对应的那一段出现。
    #[test]
    fn an_unsplittable_translation_goes_to_the_first_segment_only()
     {
        let out = split_long_lines(vec![long_line_with(
            "我还是老样子",
        )]);
        assert!(out.len() > 1);
        assert_eq!(
            out[0].translation.as_deref(),
            Some("我还是老样子")
        );
        for segment in &out[1..] {
            assert_eq!(segment.translation, None);
        }
    }

    /// 没有译文的行,切出来的每一段也都没有译文。
    #[test]
    fn a_missing_translation_stays_missing_across_segments()
    {
        let out = split_long_lines(vec![dto_line(
            0, 4_000, LONG,
        )]);
        assert!(out.len() > 1);
        for segment in &out {
            assert_eq!(segment.translation, None);
        }
    }

    /// 不需要切的行,译文原样保留。
    #[test]
    fn unsplit_lines_keep_their_translation() {
        let mut line = dto_line(0, 1_000, "短短一行");
        line.translation =
            Some(LONG_TRANSLATION.to_owned());
        assert_eq!(
            split_long_lines(vec![line.clone()]),
            vec![line]
        );
    }

    /// 中文译文的切点落在 UTF-8 字符边界上,不 panic、不产生半个字。
    #[test]
    fn translation_split_does_not_break_multibyte_chars() {
        let translation =
            "我弹着吉他,盼着明天,能说一句我很好,".repeat(4);
        let out = split_long_lines(vec![long_line_with(
            &translation,
        )]);
        assert!(out.len() > 1);
        assert_eq!(joined_translation(&out), translation);
        for segment in &out {
            assert_ne!(
                segment.translation.as_deref(),
                Some("")
            );
        }
    }

    /// 切点落在 UTF-8 字符边界上,整行中文不 panic。
    #[test]
    fn split_does_not_break_multibyte_chars() {
        let text =
            "我弹着吉他,盼着明天,能说一句我很好,".repeat(6);
        let out = split_long_lines(vec![dto_line(
            0, 4_000, &text,
        )]);
        assert!(out.len() > 1);
        for segment in &out {
            assert!(!segment.text.is_empty());
        }
    }

    /// 空行与纯空白:不 panic,也不产生空段。
    #[test]
    fn empty_lines_are_safe() {
        let lines = vec![
            dto_line(0, 1_000, ""),
            dto_line(1_000, 2_000, "   "),
        ];
        assert_eq!(split_long_lines(lines.clone()), lines);
    }

    /// 造一行上游歌词。
    fn proto_line(
        start_ms: i64,
        text: &str,
        translation: &str,
    ) -> proto::LyricLine {
        proto::LyricLine {
            start_ms,
            end_ms: start_ms + 200,
            text: text.to_owned(),
            words: Vec::new(),
            translation: translation.to_owned(),
            romaji: String::new(),
        }
    }

    /// 逐行歌词:时刻与文本原样过来,顺序不变。
    #[test]
    fn lyric_maps_lines_in_order() {
        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Line as i32,
            lines: vec![
                proto_line(1_000, "第一句", "first"),
                proto_line(5_000, "第二句", "second"),
            ],
        });

        assert_eq!(dto.lines.len(), 2);
        assert_eq!(dto.lines[0].start_ms, 1_000);
        assert_eq!(dto.lines[0].end_ms, 1_200);
        assert_eq!(dto.lines[0].text, "第一句");
        assert_eq!(
            dto.lines[0].translation.as_deref(),
            Some("first")
        );
        assert_eq!(dto.lines[1].text, "第二句");
    }

    /// 纯音乐/未收录:上游给空歌词,这里必须是**空行表**而不是失败 ——
    /// 「这首歌没有歌词」是正常状态。
    #[test]
    fn empty_lyric_yields_empty_lines() {
        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Line as i32,
            lines: Vec::new(),
        });
        assert!(dto.lines.is_empty());
    }

    /// 没有译文的行:空串翻成 None,不用空串冒充「有一句空翻译」。
    #[test]
    fn line_without_translation_omits_it() {
        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Line as i32,
            lines: vec![proto_line(0, "只有原文", "")],
        });
        assert_eq!(dto.lines[0].translation, None);
    }

    /// 逐字档位:上游已拼好整行文本,行级消费方拿到的东西与逐行档位一致。
    #[test]
    fn word_timed_lyric_still_yields_line_text() {
        let mut line = proto_line(2_000, "拼好的整行", "");
        line.words = vec![
            proto::LyricWord {
                start_ms: 2_000,
                end_ms: 2_300,
                text: "拼好的".to_owned(),
            },
            proto::LyricWord {
                start_ms: 2_300,
                end_ms: 2_600,
                text: "整行".to_owned(),
            },
        ];

        let dto = lyric_to_dto(proto::Lyric {
            timing: proto::LyricTiming::Word as i32,
            lines: vec![line],
        });

        assert_eq!(dto.lines.len(), 1);
        assert_eq!(dto.lines[0].text, "拼好的整行");
        assert_eq!(dto.lines[0].start_ms, 2_000);
    }

    /// 构造一首字段齐全的上游歌曲,各测试再按需改动其中一两个字段。
    fn full_track() -> proto::Track {
        proto::Track {
            platform: proto::Platform::Netease as i32,
            id: "1974443814".to_owned(),
            title: "紅蓮華".to_owned(),
            alias: "鬼滅の刃 OP".to_owned(),
            artists: vec![proto::Artist {
                id: "12345".to_owned(),
                name: "LiSA".to_owned(),
                ..Default::default()
            }],
            album: Some(proto::Album {
                id: "88888".to_owned(),
                name: "LiSA BEST".to_owned(),
                cover: "https://p1.music.126.net/cover.jpg"
                    .to_owned(),
                ..Default::default()
            }),
            duration_ms: 234_000,
            cover: "https://p1.music.126.net/cover.jpg"
                .to_owned(),
            quality: None,
            fee: proto::Fee::Free as i32,
        }
    }

    /// 歌曲身份与核心字段必须原样落到 DTO:平台、id、标题、时长、封面一个不丢、一个不改。
    #[test]
    fn track_maps_identity_and_core_fields() {
        let dto = track_to_dto(full_track());

        assert_eq!(dto.platform, "netease");
        assert_eq!(dto.id, "1974443814");
        assert_eq!(dto.title, "紅蓮華");
        assert_eq!(dto.duration_ms, 234_000);
        assert_eq!(
            dto.cover.as_deref(),
            Some("https://p1.music.126.net/cover.jpg")
        );
    }

    /// 多个歌手保持列表形态,服务端不拼字符串 —— 用「/」还是「&」拼是显示问题,属于 UI。
    #[test]
    fn track_keeps_artists_as_list() {
        let mut track = full_track();
        track.artists = vec![
            proto::Artist {
                name: "LiSA".to_owned(),
                ..Default::default()
            },
            proto::Artist {
                name: "梶浦由記".to_owned(),
                ..Default::default()
            },
        ];

        let dto = track_to_dto(track);

        assert_eq!(dto.artists, vec!["LiSA", "梶浦由記"]);
    }

    /// 平台没给歌手就是空列表,不编造「未知歌手」—— 缺失就是缺失,由客户端决定怎么显示。
    #[test]
    fn track_without_artists_yields_empty_list() {
        let mut track = full_track();
        track.artists = vec![];

        let dto = track_to_dto(track);

        assert!(dto.artists.is_empty());
    }

    /// 上游的封面来自专辑,没有专辑就没有封面。此时 DTO 给 `None` 而不是空串 ——
    /// 客户端拿到 `Some("")` 会当成一个真实地址去加载,拿到 `None` 才会走占位图。
    #[test]
    fn track_without_album_omits_cover() {
        let mut track = full_track();
        track.album = None;
        track.cover = String::new();

        let dto = track_to_dto(track);

        assert_eq!(dto.cover, None);
    }

    /// 别名是可选的。空 alias 翻成 `None`,免得客户端在标题下面渲染一行空副标题。
    #[test]
    fn track_alias_is_optional() {
        let with_alias = track_to_dto(full_track());
        assert_eq!(
            with_alias.alias.as_deref(),
            Some("鬼滅の刃 OP")
        );

        let mut track = full_track();
        track.alias = String::new();
        assert_eq!(track_to_dto(track).alias, None);
    }

    /// 播放源的直链、格式、码率照搬,`trial` 必须一并送出 ——
    /// 不告诉客户端这只是试听片段的话,歌放到 30 秒就停会被当成播放器坏了。
    #[test]
    fn play_source_maps_url_and_trial_flag() {
        let source = proto::PlaySource {
            url: "https://m8.music.126.net/x.mp3"
                .to_owned(),
            format: "mp3".to_owned(),
            size: 8_000_000,
            bit_rate: 320_000,
            level: proto::QualityLevel::Standard as i32,
            trial: true,
        };

        let dto = play_source_to_dto(source);

        assert_eq!(
            dto.url,
            "https://m8.music.126.net/x.mp3"
        );
        assert_eq!(dto.format, "mp3");
        assert_eq!(dto.bit_rate, 320_000);
        assert!(dto.trial);
    }

    /// 构造出的请求带上了 x-user-id —— 上游靠它选凭据,漏了那条路由整个不可用。
    #[test]
    fn request_carries_the_user_id_in_metadata() {
        let account = Account {
            id: 42,
            username: "alice".to_owned(),
        };

        let request = as_user(
            &account,
            proto::GetTracksRequest::default(),
        );

        assert_eq!(
            request
                .metadata()
                .get("x-user-id")
                .map(|value| value.to_str().unwrap()),
            Some("42"),
        );
    }

    /// 加 metadata 不动消息体。
    #[test]
    fn request_body_is_untouched() {
        let account = Account {
            id: 1,
            username: "alice".to_owned(),
        };
        let message = proto::GetTracksRequest {
            platform: proto::Platform::Netease as i32,
            track_ids: vec!["1".to_owned(), "2".to_owned()],
        };

        let request = as_user(&account, message.clone());

        assert_eq!(request.into_inner(), message);
    }

    /// 歌手的详情字段都过桥;头像空串翻成 None,不用空串冒充"有头像"。
    #[test]
    fn artist_to_dto_maps_the_detail_fields() {
        let dto = artist_to_dto(proto::Artist {
            id: "11127".to_owned(),
            name: "本兮".to_owned(),
            avatar: "https://p1.music.126.net/a.jpg"
                .to_owned(),
            description: String::new(),
            album_count: 24,
        });

        assert_eq!(dto.id, "11127");
        assert_eq!(dto.name, "本兮");
        assert_eq!(
            dto.avatar.as_deref(),
            Some("https://p1.music.126.net/a.jpg")
        );
        assert_eq!(dto.album_count, 24);

        let no_avatar = artist_to_dto(proto::Artist {
            id: "1".to_owned(),
            name: "甲".to_owned(),
            ..Default::default()
        });
        assert_eq!(no_avatar.avatar, None);
    }

    /// 走这条路的歌单一律标成平台来源 —— 本地歌单从不经过这里。
    /// 标错的现象是界面把删除键画在了平台歌单上。
    #[test]
    fn playlist_to_dto_tags_the_platform_source() {
        let dto = playlist_to_dto(proto::Playlist {
            id: "24381616".to_owned(),
            name: "华语经典".to_owned(),
            track_count: 120,
            ..Default::default()
        });

        assert_eq!(dto.source, PlaylistSource::Platform);
        assert_eq!(dto.id, "24381616");
        assert_eq!(dto.track_count, 120);
        // 封面缺席时是 None
        assert_eq!(dto.cover, None);
    }

    /// 红心歌单不进平台那半张列表。
    ///
    /// bang-dream 从此原样透传它(见 `docs/adr/0022`),排除的责任转移到了这边。
    /// 漏掉这一步的现象是界面上并排站着两个「我喜欢的」,而它们连 id 都不一样。
    #[test]
    fn the_liked_playlist_is_dropped_from_the_platform_half()
     {
        let got = platform_playlists_to_dto(vec![
            proto::Playlist {
                id: "403421443".to_owned(),
                name: "青城叶北喜欢的音乐".to_owned(),
                special_type: 5,
                ..Default::default()
            },
            proto::Playlist {
                id: "17627306389".to_owned(),
                name: "favorite_1".to_owned(),
                ..Default::default()
            },
        ]);

        assert_eq!(
            got.len(),
            1,
            "红心歌单不该进平台那半张"
        );
        assert_eq!(got[0].id, "17627306389");
    }

    /// 别的特殊类型一个都不能误伤:年度歌单(`special_type` 20)是真歌单。
    /// 按「非零就排除」写会把它一起吃掉 —— 这个坑上游踩过一次。
    #[test]
    fn other_special_types_stay_in_the_platform_half() {
        let got = platform_playlists_to_dto(vec![
            proto::Playlist {
                id: "13053579003".to_owned(),
                name: "青城叶北的2024年度歌单".to_owned(),
                special_type: 20,
                ..Default::default()
            },
        ]);

        assert_eq!(got.len(), 1, "年度歌单是真歌单");
    }

    /// 边界:`special_type` 缺席落到 0,那是普通歌单,要留下。
    #[test]
    fn a_playlist_without_a_special_type_is_ordinary() {
        let got = platform_playlists_to_dto(vec![
            proto::Playlist {
                id: "812552458".to_owned(),
                name: "anime".to_owned(),
                ..Default::default()
            },
        ]);

        assert_eq!(got.len(), 1);
    }

    fn dto(id: &str) -> TrackDto {
        TrackDto {
            platform: "netease".to_owned(),
            id: id.to_owned(),
            title: id.to_owned(),
            alias: None,
            artists: Vec::new(),
            cover: None,
            duration_ms: 1,
        }
    }

    fn track_ref(id: &str) -> TrackRef {
        TrackRef::new(id, None)
    }

    /// 快路径:歌单详情一次把每首的详情都给全了,一次补拉都不必发。
    #[test]
    fn nothing_is_backfilled_when_detail_covers_every_ref()
    {
        let missing = refs_missing_from(
            &[dto("1"), dto("2")],
            &[track_ref("1"), track_ref("2")],
        );

        assert!(
            missing.is_empty(),
            "详情够全时不该有任何补拉,实际 {missing:?}"
        );
    }

    /// 平台把 `tracks` 截断时,只对差额补拉 —— 不是整份重取。
    ///
    /// 这正是不敢直接吃 `tracks` 的理由:`trackIds` 是全量的,`tracks` 不是。
    #[test]
    fn only_the_refs_missing_from_detail_are_backfilled() {
        let missing = refs_missing_from(
            &[dto("1"), dto("3")],
            &[
                track_ref("1"),
                track_ref("2"),
                track_ref("3"),
                track_ref("4"),
            ],
        );

        assert_eq!(
            missing,
            vec!["2".to_owned(), "4".to_owned()],
            "只该补详情里没有的那些,且保持 refs 的次序"
        );
    }

    /// 常态:每条成员关系都有详情,一条都不剔,报 0。
    #[test]
    fn nothing_is_dropped_when_every_ref_has_details() {
        let (known, dropped) = keep_available(
            &[track_ref("1"), track_ref("2")],
            &HashSet::new(),
        );

        assert_eq!(known.len(), 2);
        assert_eq!(dropped, 0, "常态就是这一条");
    }

    /// 剔掉的正是拿不到详情的那些,剩下的保持原次序。
    #[test]
    fn only_the_refs_without_details_are_dropped() {
        let unavailable: HashSet<String> =
            ["2".to_owned()].into_iter().collect();

        let (known, dropped) = keep_available(
            &[
                track_ref("1"),
                track_ref("2"),
                track_ref("3"),
            ],
            &unavailable,
        );

        let ids: Vec<&str> =
            known.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["1", "3"], "过滤不该重排");
        assert_eq!(dropped, 1);
    }

    /// 报出来的条数是**这个歌单里**少的那些,不是那个集合的大小。
    ///
    /// 集合里可能有不属于这个歌单的 id(它是按整批 id 问出来的)。拿集合大小
    /// 当条数,用户会在这张列表上看到一个永远对不上的数字。
    #[test]
    fn the_dropped_count_ignores_ids_from_other_playlists()
    {
        let unavailable: HashSet<String> =
            ["2".to_owned(), "别的歌单里的".to_owned()]
                .into_iter()
                .collect();

        let (_known, dropped) = keep_available(
            &[track_ref("1"), track_ref("2")],
            &unavailable,
        );

        assert_eq!(
            dropped, 1,
            "只该数这个歌单里真的少掉的那一条"
        );
    }

    /// 边界:平台一条 `tracks` 都没给(整份被截断),那就全部补拉。
    #[test]
    fn an_empty_detail_backfills_every_ref() {
        let missing = refs_missing_from(
            &[],
            &[track_ref("1"), track_ref("2")],
        );

        assert_eq!(
            missing,
            vec!["1".to_owned(), "2".to_owned()]
        );
    }
}
