//! 字体资产自检:界面只有一族「Osmosis Sans」(docs/design.md「字体」)。
//!
//! 桌面那三件是 Figtree 与 Noto Sans CJK SC 按标准字符集合并的成品,安卓那三件只有
//! Figtree(中文走系统的 Noto Sans CJK)。重新生成走 `just font-subset`。
//! 歌名、歌手名是平台给的任意文字,不在这里守:子集按标准字符集裁,不按今天的曲库裁,
//! 漏字看 `just font-subset` 末尾那行覆盖报告。

use std::path::Path;

const WEIGHTS: [u16; 3] = [400, 600, 700];

fn font(path: &str) -> Vec<u8> {
    let full =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read(&full).unwrap_or_else(|e| {
        panic!(
            "字体文件 {} 应存在(重跑 just font-subset):{e}",
            full.display()
        )
    })
}

fn faces() -> Vec<(String, Vec<u8>)> {
    ["desktop", "android"]
        .iter()
        .flat_map(|side| {
            WEIGHTS.iter().map(move |w| {
                let name = format!(
                    "fonts/{side}/osmosis-sans-{w}.ttf"
                );
                let bytes = font(&name);
                (name, bytes)
            })
        })
        .collect()
}

fn missing(
    face: &ttf_parser::Face,
    text: impl Iterator<Item = char>,
) -> Vec<char> {
    let mut out: Vec<char> = text
        .filter(|c| {
            !c.is_whitespace()
                && face.glyph_index(*c).is_none()
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// 每一件都叫 Osmosis Sans,字重对得上,ASCII 可见区不缺字。
#[test]
fn every_face_is_osmosis_sans_and_covers_ascii() {
    for (name, bytes) in faces() {
        let face = ttf_parser::Face::parse(&bytes, 0)
            .unwrap_or_else(|e| {
                panic!("{name} 应能解析:{e}")
            });
        let family = face
            .names()
            .into_iter()
            .find(|n| {
                n.name_id == ttf_parser::name_id::TYPOGRAPHIC_FAMILY
                    && n.is_unicode()
            })
            .and_then(|n| n.to_string());
        assert_eq!(
            family.as_deref(),
            Some("Osmosis Sans"),
            "{name}"
        );
        assert!(
            name.contains(
                &face.weight().to_number().to_string()
            ),
            "{name} 的 usWeightClass 是 {}",
            face.weight().to_number()
        );
        let lost =
            missing(&face, (0x20u8..0x7f).map(char::from));
        assert!(lost.is_empty(), "{name} 缺字形:{lost:?}");
    }
}

/// 数字是等宽的:tnum 冻进了 cmap,时长与「1:13 / 4:14」不靠等宽字体也对得齐。
#[test]
fn digits_are_tabular() {
    for (name, bytes) in faces() {
        let face =
            ttf_parser::Face::parse(&bytes, 0).unwrap();
        let widths: Vec<u16> = ('0'..='9')
            .map(|c| {
                let id = face.glyph_index(c).unwrap();
                face.glyph_hor_advance(id).unwrap()
            })
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "{name} 的数字宽度不一:{widths:?}"
        );
    }
}
