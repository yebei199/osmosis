//! 色板的对比度。无头跑,与 controls.rs 同一套路。
//!
//! 这些断言盖的是**眼睛盖不住的那一类错**:深浅两套里同一对前景/背景,人只会
//! 逐屏看自己想得到的那几处,而算术不会漏。本文件的由来正是漏掉的那两处 ——
//! `accent-text` 在浅色下压浅底(整排控制键消失),以及播放页的深字压深底
//! (歌名 1.23:1)。两次都是改完很久才被看见的。
//!
//! 判据取 WCAG AA 的 4.5:1(正文)。大字本可放宽到 3:1,但这里不分大小 ——
//! 一个色板 token 会被用在多大的字上,不由色板决定。
//!
//! **盖不住什么,先说清楚:**
//!
//! - **派生色**。界面上不少颜色是 `accent.darker(0.4)` / `overlay.with-alpha(..)`
//!   这样现算的(HoverButton 的三态底就是),而这里只读得到色板上那些具名 token。
//!   派生那一档仍然只能靠眼睛。
//! - **半透明叠在什么上**。`surface.with-alpha(0.67)` 的实际观感取决于它压着谁,
//!   而那由布局决定,不由色板决定。
//! - **好不好看**。4.5:1 只保证读得出来。

use ui::{MainWindow, Theme};

use i_slint_backend_testing as testing;
use slint::ComponentHandle;

/// WCAG 的相对亮度。
fn luminance(c: slint::Color) -> f32 {
    let channel = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.red())
        + 0.7152 * channel(c.green())
        + 0.0722 * channel(c.blue())
}

/// 半透明的前景叠在背景上之后的颜色(沉浸层的弱字是带 alpha 的白)。
fn over(
    fg: slint::Color,
    bg: slint::Color,
) -> slint::Color {
    let a = f32::from(fg.alpha()) / 255.0;
    let mix = |f: u8, b: u8| {
        (f32::from(f) * a + f32::from(b) * (1.0 - a))
            .round() as u8
    };
    slint::Color::from_rgb_u8(
        mix(fg.red(), bg.red()),
        mix(fg.green(), bg.green()),
        mix(fg.blue(), bg.blue()),
    )
}

/// WCAG 的对比度,1.0 到 21.0。前景带 alpha 时先叠到背景上。
fn contrast(fg: slint::Color, bg: slint::Color) -> f32 {
    let (a, b) = (luminance(over(fg, bg)), luminance(bg));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// WCAG AA 对正文的要求。
const AA: f32 = 4.5;

/// 一对前景/背景,以及它在界面上是什么,要求多少。
struct Pair {
    what: &'static str,
    min: f32,
    fg: fn(&Theme) -> slint::Color,
    bg: fn(&Theme) -> slint::Color,
}

/// 建一个窗口。
///
/// `init_no_event_loop` 在**同一条线程里只能调一次**(第二次报
/// `platform already initialized`),所以一条测试只建一个窗口,深浅两套靠
/// 拨 `dark` 切 —— 色板那些 `out` 属性都是它的绑定,拨完当场重算。
fn window() -> MainWindow {
    testing::init_no_event_loop();
    MainWindow::new().expect("建不出主窗口")
}

/// 把一组配对在深浅两套下各量一遍,把不达标的收集成人话。
fn failures(
    ui: &MainWindow,
    pairs: &[Pair],
) -> Vec<String> {
    let mut bad = Vec::new();
    for dark in [true, false] {
        ui.global::<Theme>().set_dark(dark);
        let theme = ui.global::<Theme>();
        let which = if dark { "深色" } else { "浅色" };
        for p in pairs {
            let ratio =
                contrast((p.fg)(&theme), (p.bg)(&theme));
            if ratio < p.min {
                bad.push(format!(
                    "{which}下「{}」只有 {ratio:.2}:1,要 {}:1",
                    p.what, p.min
                ));
            }
        }
    }
    bad
}

/// **常驻界面的字在两套主题下都读得出来。**
///
/// 会翻的前景压会翻的背景,两边各翻各的 —— 一边对了不代表另一边对。
/// 浅色下整排控制键消失过一次,就是这么来的:那个 token 深色下恰好同值。
#[test]
fn chrome_text_is_legible_in_both_themes() {
    let pairs = [
        Pair {
            min: AA,
            what: "主字 / 音乐页底",
            fg: |t| t.get_text(),
            bg: |t| t.get_base(),
        },
        Pair {
            min: AA,
            what: "主字 / 卡片面",
            fg: |t| t.get_text(),
            bg: |t| t.get_surface(),
        },
        Pair {
            min: AA,
            what: "弱字 / 音乐页底",
            fg: |t| t.get_text_dim(),
            bg: |t| t.get_base(),
        },
        Pair {
            min: AA,
            what: "弱字 / 卡片面",
            fg: |t| t.get_text_dim(),
            bg: |t| t.get_surface(),
        },
        Pair {
            // 绿色 accent 的明度远高于琥珀,近白字压上去只有约 1.9:1,
            // 所以 accent-text 反转成了深墨(docs/adr/0023)。这对就是那次
            // 反转的守卫:再把它改回浅字,这里当场红。
            min: AA,
            what: "强调实底上的字 / 强调底",
            fg: |t| t.get_accent_text(),
            bg: |t| t.get_accent(),
        },
        Pair {
            min: AA,
            what: "控制键图标 / 卡片面",
            fg: |t| t.get_accent_ink(),
            bg: |t| t.get_surface(),
        },
        Pair {
            min: AA,
            what: "在放那一行的歌名 / 浮起的表面",
            fg: |t| t.get_accent_ink(),
            bg: |t| t.get_raised(),
        },
    ];

    let bad = failures(&window(), &pairs);

    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// **沉浸层的字不跟主题翻,所以两套下必须是同一个(合格的)值。**
///
/// 播放页那块底永远是深的(它托着封面点云),压在上面的字因此也得钉死。
/// 用会翻的 token 的话,浅色主题下就是深字压深底 —— 歌名曾经是 1.23:1。
#[test]
fn immersive_text_does_not_follow_the_theme() {
    let pairs = [
        Pair {
            min: AA,
            what: "歌名 / 沉浸底",
            fg: |t| t.get_immersive_text(),
            bg: |t| t.get_immersive(),
        },
        Pair {
            min: AA,
            what: "歌手与译文 / 沉浸底",
            fg: |t| t.get_immersive_dim(),
            bg: |t| t.get_immersive(),
        },
        Pair {
            min: AA,
            what: "加载提示 / 沉浸底",
            fg: |t| t.get_immersive_accent(),
            bg: |t| t.get_immersive(),
        },
        Pair {
            min: AA,
            what: "条上的歌手 / 沉浸层的控制条",
            fg: |t| t.get_immersive_dim(),
            bg: |t| t.get_immersive_raised(),
        },
    ];

    let ui = window();
    let bad = failures(&ui, &pairs);
    assert!(bad.is_empty(), "{}", bad.join("\n"));

    // 光是"两边都合格"还不够:两边取值必须**相同**。不同就说明它跟着主题
    // 翻了,而那正是要禁的事 —— 底是钉死的,字跟着翻迟早翻到底色那一侧去。
    for (what, get) in [
        (
            "immersive",
            (|t: &Theme| t.get_immersive())
                as fn(&Theme) -> slint::Color,
        ),
        ("immersive-raised", |t| t.get_immersive_raised()),
        ("immersive-text", |t| t.get_immersive_text()),
        ("immersive-dim", |t| t.get_immersive_dim()),
        ("immersive-faint", |t| t.get_immersive_faint()),
        ("immersive-accent", |t| t.get_immersive_accent()),
        ("immersive-line", |t| t.get_immersive_line()),
    ] {
        ui.global::<Theme>().set_dark(true);
        let in_dark = get(&ui.global::<Theme>());
        ui.global::<Theme>().set_dark(false);
        let in_light = get(&ui.global::<Theme>());
        assert_eq!(
            in_dark, in_light,
            "{what} 两套主题下该是同一个值 —— 沉浸层不跟主题翻"
        );
    }
}

/// **最弱那一档字只做读数,判据 3:1。**
///
/// `text-faint` 只给时长、计数、日期这类有别的路径也读得到的数字用,从不单独承载
/// 意义(docs/design.md「颜色」),所以按 WCAG 对非正文的 3:1 量,而不是 4.5:1。
#[test]
fn the_faintest_text_is_legible() {
    let pairs = [Pair {
        what: "最弱字 / 内容面板",
        min: 3.0,
        fg: |t| t.get_text_faint(),
        bg: |t| t.get_base(),
    }];

    let bad = failures(&window(), &pairs);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// 出错提示那颗图标:压在它自己那一圈底上,也压在浮起的提示上,都要读得出来。
#[test]
fn the_danger_icon_is_legible_in_both_themes() {
    let pairs = [Pair {
        what: "出错图标 / 提示的底",
        min: 3.0,
        fg: |t| t.get_danger(),
        bg: |t| t.get_raised(),
    }];

    let bad = failures(&window(), &pairs);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
