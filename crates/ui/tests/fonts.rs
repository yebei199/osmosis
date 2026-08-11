//! 字体资产的覆盖自检:标题字体必须盖住每一个硬编码的中文标题,
//! 拉丁三件必须盖住 ASCII 可见区。
//!
//! 设计稿的 display 字体 Caprasimo 没有中文字形,中文大标题走思源宋体
//! (系统里的名字是 Noto Serif CJK SC)的 Heavy 子集。子集是预裁的,
//! 新增页面标题而忘了重跑 `just font-title-subset`,这里就红,
//! 并报出缺的是哪个字。动态文本(歌名、歌手名)不在此列:
//! 它们是平台给的任意 CJK,走系统字体回退。

use std::path::Path;

/// 界面上所有硬编码的中文标题与区块题。每上一个新页面,把它的标题加进来,
/// 然后重跑 `just font-title-subset`。
const CJK_TITLES: &[&str] = &[
    // Home
    "今天做点什么",
    "装一个",
    // 音乐页与次级入口
    "每日推荐",
    "我的歌单",
    "搜索",
    "最近播放",
    "红心",
    "新建歌单",
    // 空状态
    "点一首歌开始",
    // 设置
    "设置",
    "外观",
    "账号与授权",
    "缓存与存储",
    "快捷键",
    "关于",
    // 个人主页
    "个人主页",
    "常听歌手",
    "已连接平台",
    "同播设备名册",
];

fn font(path: &str) -> Vec<u8> {
    let full =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read(&full).unwrap_or_else(|e| {
        panic!("字体文件 {} 应存在(重跑 just 里的字体配方):{e}", full.display())
    })
}

fn assert_covers(
    font_bytes: &[u8],
    name: &str,
    text: impl Iterator<Item = char>,
) {
    let face = ttf_parser::Face::parse(font_bytes, 0)
        .unwrap_or_else(|e| {
            panic!("{name} 应能被解析:{e}")
        });
    let missing: Vec<char> = text
        .filter(|c| {
            !c.is_whitespace()
                && face.glyph_index(*c).is_none()
        })
        .collect();
    assert!(
        missing.is_empty(),
        "{name} 缺字形:{missing:?}"
    );
}

/// 中文标题子集盖住全部硬编码标题。
#[test]
fn title_subset_covers_every_hardcoded_title() {
    let bytes = font("fonts/cjk-title-subset.otf");
    assert_covers(
        &bytes,
        "cjk-title-subset.otf",
        CJK_TITLES.iter().flat_map(|s| s.chars()),
    );
}

/// 拉丁三件各自盖住 ASCII 可见区:标题、正文、等宽读数都不许缺字。
#[test]
fn latin_fonts_cover_visible_ascii() {
    for name in [
        "fonts/caprasimo.ttf",
        "fonts/figtree-400.ttf",
        "fonts/figtree-600.ttf",
        "fonts/figtree-700.ttf",
        "fonts/dm-mono.ttf",
    ] {
        let bytes = font(name);
        assert_covers(
            &bytes,
            name,
            (0x20u8..0x7f).map(char::from),
        );
    }
}
