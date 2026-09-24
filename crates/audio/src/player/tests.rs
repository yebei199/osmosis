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

/// 从中间开始放的那一路,第一个出声的采样就是那个位置的 —— 开头一个采样都不漏。
///
/// 迁移时目标从源停下的那一毫秒接着放(#137 ③)。先放再跳的话,跳转生效前那几
/// 毫秒会从 0:00 响出来。这里用一段值随下标单调增长的采样(第 i 个就是 i/总数),
/// 于是「第一个非零采样的值」直接读出它来自哪里。
#[test]
fn starting_from_a_position_never_plays_the_beginning() {
    use std::num::NonZero;
    use std::time::{Duration, Instant};

    const RATE: u32 = 1_000;
    const LEN: usize = 2_000;
    let ramp: Vec<f32> =
        (0..LEN).map(|i| i as f32 / LEN as f32).collect();
    let source = rodio::buffer::SamplesBuffer::new(
        NonZero::new(1).expect("单声道"),
        NonZero::new(RATE).expect("采样率"),
        ramp,
    );
    let shared = SyncShared::new();
    let (player, output) = rodio::Player::new();

    // 拉采样的那一头就是声卡,得在另一条线程上一直拉着。
    let first_sound = std::thread::spawn(move || {
        let deadline =
            Instant::now() + Duration::from_secs(5);
        let mut output = output;
        while Instant::now() < deadline {
            match output.next() {
                Some(sample) if sample != 0.0 => {
                    return Some(sample);
                }
                Some(_) => {}
                None => return None,
            }
        }
        None
    });

    let source = start_from(
        SyncSource::new(SourceFeed(source), shared.clone()),
        &shared,
        Duration::from_millis(1_000),
        true,
    )
    .expect("跳到曲中该成功");
    player.append(source);
    player.play();

    let first = first_sound
        .join()
        .expect("拉采样的线程不该崩")
        .expect("该有声音出来");
    assert!(
        (0.499..=0.51).contains(&first),
        "第一个出声的采样是 {first},该是 1 秒处(0.5)的那一个"
    );
}

/// 按「停在那里」交进去的那一路一个采样都不出声,直到有人按播放。
#[test]
fn a_paused_start_stays_silent() {
    use std::num::NonZero;
    use std::time::{Duration, Instant};

    let source = rodio::buffer::SamplesBuffer::new(
        NonZero::new(1).expect("单声道"),
        NonZero::new(1_000).expect("采样率"),
        vec![0.25_f32; 2_000],
    );
    let shared = SyncShared::new();
    let (player, output) = rodio::Player::new();
    let heard = std::thread::spawn(move || {
        let deadline =
            Instant::now() + Duration::from_millis(300);
        let mut output = output;
        while Instant::now() < deadline {
            if output
                .next()
                .is_some_and(|sample| sample != 0.0)
            {
                return true;
            }
        }
        false
    });

    let source = start_from(
        SyncSource::new(SourceFeed(source), shared.clone()),
        &shared,
        Duration::from_millis(500),
        false,
    )
    .expect("跳到曲中该成功");
    player.append(source);
    player.play();

    assert!(
        !heard.join().expect("拉采样的线程不该崩"),
        "暂停着交进去的不该出声"
    );
    assert!(shared.is_paused(), "暂停归同步源,播放器本身一直在放");
}
