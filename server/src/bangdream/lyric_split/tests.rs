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
    let out =
        split_long_lines(vec![dto_line(0, 4_000, LONG)]);
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
fn joined_translation(lines: &[LyricLineDto]) -> String {
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
    assert_eq!(joined_translation(&out), LONG_TRANSLATION);
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
fn a_missing_translation_stays_missing_across_segments() {
    let out =
        split_long_lines(vec![dto_line(0, 4_000, LONG)]);
    assert!(out.len() > 1);
    for segment in &out {
        assert_eq!(segment.translation, None);
    }
}

/// 不需要切的行,译文原样保留。
#[test]
fn unsplit_lines_keep_their_translation() {
    let mut line = dto_line(0, 1_000, "短短一行");
    line.translation = Some(LONG_TRANSLATION.to_owned());
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
    let out =
        split_long_lines(vec![dto_line(0, 4_000, &text)]);
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
