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
    assert!(
        shared.is_paused(),
        "暂停归同步源,播放器本身一直在放"
    );
}

/// 刚开的播放器手上没有源，算暂停：声卡用不着，暂停满时限就关流(#138)。
/// 从前初值是「在放」,应用启动后一首没放，输出流就一直开着送静音，直到第一次按暂停。
#[test]
fn a_fresh_player_with_nothing_loaded_counts_as_paused() {
    let shared = fresh_shared();
    assert!(shared.is_paused(), "手上没有源就是暂停");
    assert!(shared.idle(), "暂停着，声卡用不着");
}

// ── Deck:回调那一侧不等锁(#192) ──

mod deck_side {
    use std::num::NonZero;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use rodio::buffer::SamplesBuffer;

    use super::deck::{DeckSource, deck};

    /// 48kHz 立体声的一段常数采样，换源前后格式不变，`normalize` 原样放过。
    fn constant(
        value: f32,
        frames: usize,
    ) -> SamplesBuffer {
        SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(48_000).unwrap(),
            vec![value; frames * 2],
        )
    }

    fn pull(source: &mut DeckSource, n: usize) -> Vec<f32> {
        (0..n).map(|_| source.next().unwrap()).collect()
    }

    #[test]
    fn it_plays_what_it_was_given_at_the_set_volume() {
        let (deck, mut source) = deck();
        assert_eq!(
            pull(&mut source, 2),
            vec![0.0; 2],
            "没东西放是静音"
        );
        deck.play(constant(0.5, 4));
        deck.set_volume(0.5);
        assert_eq!(pull(&mut source, 2), vec![0.25; 2]);
        assert!(!deck.empty());
    }

    #[test]
    fn clearing_or_running_out_leaves_it_empty_and_silent()
    {
        let (deck, mut source) = deck();
        deck.play(constant(0.5, 2));
        assert_eq!(pull(&mut source, 4), vec![0.5; 4]);
        assert_eq!(
            pull(&mut source, 2),
            vec![0.0; 2],
            "放完了"
        );
        assert!(deck.empty());

        deck.play(constant(0.5, 100));
        pull(&mut source, 2);
        deck.clear();
        assert!(deck.empty());
        assert_eq!(pull(&mut source, 2), vec![0.0; 2]);
    }

    /// 界面线程拿着换源的槽(正好在换歌、又被抢走了 CPU)时，回调照样拉得动，换源晚一点生效。
    #[test]
    fn pulling_never_waits_for_the_ui_thread() {
        let (deck, mut source) = deck();
        deck.play(constant(0.5, 1_000));
        pull(&mut source, 2);
        deck.play(constant(0.75, 1_000));
        let held = deck.hold_slot();

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let got = pull(&mut source, 2);
            let _ = done.send((got, source));
        });
        let (got, mut source) = finished
            .recv_timeout(Duration::from_secs(1))
            .expect("界面线程拿着槽的时候回调拉不动");
        assert_eq!(
            got,
            vec![0.5; 2],
            "换源还没取到，接着放旧的"
        );
        drop(held);
        assert_eq!(pull(&mut source, 2), vec![0.75; 2]);
    }

    /// 换下来的旧源不在回调线程上析构：析构可能要还几 MB 缓冲给系统。
    #[test]
    fn a_replaced_source_is_released_off_the_pulling_thread()
     {
        struct Dropped(
            SamplesBuffer,
            Arc<AtomicBool>,
            std::thread::ThreadId,
        );
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.1.store(
                    std::thread::current().id() == self.2,
                    Ordering::Relaxed,
                );
            }
        }
        impl Iterator for Dropped {
            type Item = f32;
            fn next(&mut self) -> Option<f32> {
                self.0.next()
            }
        }
        impl rodio::Source for Dropped {
            fn current_span_len(&self) -> Option<usize> {
                None
            }
            fn channels(&self) -> rodio::ChannelCount {
                self.0.channels()
            }
            fn sample_rate(&self) -> rodio::SampleRate {
                self.0.sample_rate()
            }
            fn total_duration(&self) -> Option<Duration> {
                None
            }
        }

        let (deck, source) = deck();
        let on_deck_thread =
            Arc::new(AtomicBool::new(false));
        deck.play(Dropped(
            constant(0.5, 1_000),
            on_deck_thread.clone(),
            std::thread::current().id(),
        ));
        let puller = std::thread::spawn(move || {
            let mut source = source;
            pull(&mut source, 2);
            source
        });
        let mut source = puller.join().unwrap();
        deck.play(constant(0.75, 1_000));
        std::thread::spawn(move || pull(&mut source, 2))
            .join()
            .unwrap();
        deck.collect_for_test(Duration::ZERO);
        assert!(
            on_deck_thread.load(Ordering::Relaxed),
            "旧源该在界面线程(Deck 这头)上释放"
        );
    }
}
