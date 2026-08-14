//! 超长歌词行的二次切分。
//!
//! 平台给的行粒度不可控,一行 198 字符的整段词推到播放页会撑破布局
//! (见 issue #21)。切分是对上游脏数据的归一,和 [`super::non_empty`] 同类,
//! 不是画法 —— 所有客户端都该拿到同样粒度的行。

use contract::LyricLineDto;

use super::non_empty;

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
pub(super) fn split_long_lines(
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
mod tests;
