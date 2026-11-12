use super::*;

/// 每一档保住自己的编号 —— `.slint` 与设置文件按下标说话,错一位就切到另一个效果。
#[test]
fn every_effect_keeps_its_index() {
    assert_eq!(Effect::from_index(0), Effect::Sleeve);
    assert_eq!(Effect::from_index(1), Effect::Cloud);
}

/// 默认档是封面实物。
#[test]
fn the_default_effect_is_the_sleeve() {
    assert_eq!(Effect::default(), Effect::Sleeve);
}

/// 越界的编号回默认档,不画一片空白。
#[test]
fn an_out_of_range_index_falls_back_to_the_default() {
    for index in [-1, -100, 2, 7, i32::MAX, i32::MIN] {
        assert_eq!(
            Effect::from_index(index),
            Effect::Sleeve,
            "编号 {index} 该回默认档"
        );
    }
}
