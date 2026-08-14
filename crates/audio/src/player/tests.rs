
use super::*;

/// 音量夹在 0..=1。
///
/// rodio 对越界值照单全收,而后果都不报错:负数是把波形反相,单独听像是
/// "声音变空了",与别的声源混在一起会互相抵消;大于 1 是数字过载削波。
/// 两种都难听,且都不会有任何一行日志说出原因。
#[test]
fn volume_is_clamped_to_a_sane_range() {
    assert!(
        (clamped_volume(0.5) - 0.5).abs() < f32::EPSILON,
        "范围内的值不该被动"
    );
    assert!(
        (clamped_volume(1.7) - 1.0).abs() < f32::EPSILON,
        "过载要收到 1.0"
    );
    assert!(
        (clamped_volume(-0.3) - 0.0).abs() < f32::EPSILON,
        "负数要收到 0.0,不能留着反相"
    );
    // NaN 的比较全为假,不特判就会原样传给 rodio
    assert!(
        (clamped_volume(f32::NAN) - 0.0).abs()
            < f32::EPSILON,
        "NaN 当静音"
    );
}
