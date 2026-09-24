//! 同步播放的规则与同步源。
//!
//! 媒体采样率一律取 1000Hz:一帧就是一毫秒，数字读得出来。媒体的第 n 帧的值就是 n,
//! 吐出来的是媒体的哪一帧一眼看得见。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate};
use similar_asserts::assert_eq;

use super::*;

const RATE: f64 = 1000.0;
const MS: i64 = 1_000_000;

// ── 时间线 ──

fn follow(at_ns: i64, media_ns: i64, playing: bool, start_ns: i64) -> Target {
    Target::Follow {
        anchor: Anchor { at_ns, media_ns },
        playing,
        start_ns,
    }
}

/// 起播之后按锚点线性往前走;起播之前、暂停着、不跟时间线，都是「不该出声」。
#[test]
fn the_timeline_maps_a_moment_to_a_media_position() {
    let target = follow(10_000 * MS, 5_000 * MS, true, 10_000 * MS);
    assert_eq!(target.desired(10_500 * MS), Some(5_500 * MS));
    assert_eq!(target.desired(9_900 * MS), None, "还没到起播");

    let paused = follow(10_000 * MS, 5_000 * MS, false, 0);
    assert_eq!(paused.desired(10_500 * MS), None, "暂停着");
    assert_eq!(Target::Free.desired(10_500 * MS), None);
}

/// 校时微调锚点时，起播那一刻不跟着动：早已开始的，锚点挪到未来也还是在放。
#[test]
fn a_later_anchor_does_not_undo_a_start_that_already_happened() {
    let target = follow(20_000 * MS, 15_000 * MS, true, 10_000 * MS);
    assert_eq!(target.desired(19_000 * MS), Some(14_000 * MS));
}

#[test]
fn until_start_counts_down_only_before_the_start() {
    let target = follow(10_000 * MS, 0, true, 10_000 * MS);
    assert_eq!(target.until_start(9_000 * MS), Some(1_000 * MS));
    assert_eq!(target.until_start(10_000 * MS), None);
    assert_eq!(follow(10_000 * MS, 0, false, 10_000 * MS).until_start(9_000 * MS), None);
    assert_eq!(Target::Free.until_start(0), None);
}

// ── 跟随器 ──

/// 已经跟上时间线的跟随器:第一块对齐之后才算在跟。
fn running(target: &Target, present_ns: i64) -> Follower {
    let mut follower = Follower::default();
    let want = target.desired(present_ns).expect("在放") as f64 * RATE / 1e9;
    let _ = follower.decide(target, present_ns, want, RATE, 10);
    let _ = follower.decide(target, present_ns, want, RATE, 10);
    follower
}

/// 不跟时间线就照放，一帧一帧往下走。
#[test]
fn free_playback_just_plays() {
    let mut follower = Follower::default();
    assert_eq!(
        follower.decide(&Target::Free, 0, 123.0, RATE, 10),
        Decision::Play { step: 1.0, muted: false }
    );
}

/// 落后得不多(缓冲里就有)就往前丢帧，落后太多就 seek,超前也 seek(退不回去)。
#[test]
fn a_large_error_skips_or_seeks() {
    let target = follow(0, 0, true, 0);
    let now = 10_000 * MS; // 该在媒体第 10 秒

    let mut behind_a_little = Follower::default();
    assert_eq!(
        behind_a_little.decide(&target, now, 9_000.0, RATE, 10),
        Decision::Skip { frames: 1_000 }
    );

    let mut behind_a_lot = Follower::default();
    assert_eq!(
        behind_a_lot.decide(&target, now, 0.0, RATE, 10),
        Decision::Seek { to_frames: 10_000.0 }
    );

    let mut ahead = Follower::default();
    assert_eq!(
        ahead.decide(&target, now, 10_100.0, RATE, 10),
        Decision::Seek { to_frames: 10_000.0 }
    );
}

/// 小误差用 ±0.1% 以内的速率慢慢追，方向对：超前就放慢，落后就加快。
#[test]
fn a_small_error_nudges_the_rate_within_bounds() {
    let target = follow(0, 0, true, 0);
    let now = 10_000 * MS;
    let mut follower = running(&target, now);

    let Decision::Play { step, muted } = follower.decide(&target, now, 10_002.0, RATE, 10) else {
        panic!("小误差该照放");
    };
    assert!(!muted, "已经对齐过了，小误差不该静音");
    assert!(step < 1.0 && step >= 1.0 - MAX_CORR, "超前 2ms 该放慢: {step}");

    let Decision::Play { step, .. } = follower.decide(&target, now, 9_998.0, RATE, 10) else {
        panic!("小误差该照放");
    };
    assert!(step > 1.0 && step <= 1.0 + MAX_CORR, "落后 2ms 该加快: {step}");
}

/// 跳过之后先静音，误差回到对齐判据以内才出声：不放出一段错位的声音。
#[test]
fn after_a_jump_it_stays_muted_until_aligned() {
    let target = follow(0, 0, true, 0);
    let now = 10_000 * MS;
    let mut follower = Follower::default();
    let _ = follower.decide(&target, now, 9_000.0, RATE, 10); // 丢帧

    assert!(matches!(
        follower.decide(&target, now, 10_003.0, RATE, 10),
        Decision::Play { muted: true, .. }
    ), "误差 3ms,还没对齐");
    assert!(matches!(
        follower.decide(&target, now, 10_001.0, RATE, 10),
        Decision::Play { muted: false, .. }
    ), "误差 1ms,对齐了");
}

/// 起播那一刻落在这一块里：先出这么多帧静音，下一帧就从起播位置开始。
#[test]
fn a_start_inside_the_block_holds_for_the_lead_frames() {
    let target = follow(10_004 * MS, 500 * MS, true, 10_004 * MS);
    let mut follower = Follower::default();
    assert_eq!(
        follower.decide(&target, 10_000 * MS, 500.0, RATE, 10),
        Decision::Hold { lead_frames: Some(4) }
    );
    assert_eq!(
        Follower::default().decide(&target, 9_000 * MS, 500.0, RATE, 10),
        Decision::Hold { lead_frames: None },
        "起播还远，这一块整块静音"
    );
}

/// 还没起播，但手上不在起播位置：先 seek 过去备着。
#[test]
fn before_the_start_it_prepares_at_the_start_position() {
    let target = follow(10_000 * MS, 500 * MS, true, 10_000 * MS);
    assert_eq!(
        Follower::default().decide(&target, 9_000 * MS, 0.0, RATE, 10),
        Decision::Seek { to_frames: 500.0 }
    );
}

/// 暂停着：停在锚点位置，不在就 seek 过去。
#[test]
fn paused_it_holds_at_the_anchor() {
    let target = follow(10_000 * MS, 500 * MS, false, 0);
    assert_eq!(
        Follower::default().decide(&target, 12_000 * MS, 500.0, RATE, 10),
        Decision::Hold { lead_frames: None }
    );
    assert_eq!(
        Follower::default().decide(&target, 12_000 * MS, 90.0, RATE, 10),
        Decision::Seek { to_frames: 500.0 }
    );
}

// ── 同步源 ──

/// 测试用的媒体：第 n 帧每个声道的值都是 n;可以指定第几次拉取欠载;记下 seek。
struct FakeFeed {
    frames: usize,
    channels: u16,
    cursor: usize,
    pulls: usize,
    starve_on: Vec<usize>,
    seeks: Arc<Mutex<Vec<Duration>>>,
}

impl FakeFeed {
    fn mono(frames: usize) -> Self {
        Self {
            frames,
            channels: 1,
            cursor: 0,
            pulls: 0,
            starve_on: Vec::new(),
            seeks: Arc::default(),
        }
    }
}

impl Feed for FakeFeed {
    fn pull(&mut self) -> Pulled {
        self.pulls += 1;
        if self.starve_on.contains(&self.pulls) {
            return Pulled::Starved;
        }
        let frame = self.cursor / usize::from(self.channels);
        if frame >= self.frames {
            return Pulled::End;
        }
        self.cursor += 1;
        Pulled::Sample(frame as f32)
    }

    fn seek(&mut self, to: Duration) -> Result<(), SeekError> {
        self.seeks.lock().unwrap().push(to);
        let frame = (to.as_secs_f64() * RATE).round() as usize;
        self.cursor = frame * usize::from(self.channels);
        Ok(())
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(self.channels).unwrap()
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(RATE as u32).unwrap()
    }
}

fn take(source: &mut impl Iterator<Item = f32>, n: usize) -> Vec<f32> {
    (0..n).map(|_| source.next().expect("还没放完")).collect()
}

fn values(from: usize, n: usize) -> Vec<f32> {
    (from..from + n).map(|v| v as f32).collect()
}

/// 不跟时间线：媒体原样往下放，位置跟着走。
#[test]
fn free_playback_passes_the_media_through() {
    let shared = SyncShared::new();
    let mut source = SyncSource::new(FakeFeed::mono(100), shared.clone());

    assert_eq!(take(&mut source, 10), values(0, 10));
    assert_eq!(shared.position(), Duration::from_millis(9));
}

/// 多声道按帧交错，一帧的几个声道一起走。
#[test]
fn channels_stay_interleaved() {
    let shared = SyncShared::new();
    let feed = FakeFeed { channels: 2, ..FakeFeed::mono(100) };
    let mut source = SyncSource::new(feed, shared);

    assert_eq!(take(&mut source, 6), vec![0.0, 0.0, 1.0, 1.0, 2.0, 2.0]);
}

/// 欠载时吐静音，但位置不往前走：那段时间媒体一帧都没放。
#[test]
fn a_starved_feed_does_not_advance_the_position() {
    let shared = SyncShared::new();
    let feed = FakeFeed { starve_on: vec![4, 5], ..FakeFeed::mono(100) };
    let mut source = SyncSource::new(feed, shared.clone());

    assert_eq!(take(&mut source, 7), vec![0.0, 1.0, 2.0, 0.0, 0.0, 3.0, 4.0]);
    assert_eq!(shared.position(), Duration::from_millis(4));
    assert_eq!(shared.report().starves, 1, "连着两次拉不到算一次欠载");
}

/// 暂停：不出声、不消耗媒体;继续之后从停下的地方接着放。
#[test]
fn pausing_holds_the_media_in_place() {
    let shared = SyncShared::new();
    let mut source = SyncSource::new(FakeFeed::mono(100), shared.clone());
    take(&mut source, 3);

    shared.pause();
    assert!(shared.is_paused());
    assert_eq!(take(&mut source, 4), vec![0.0; 4]);

    shared.resume();
    assert_eq!(take(&mut source, 2), values(3, 2));
}

/// 跳转请求在下一次被拉时执行，裁决如实回来，之后吐的就是那一刻的媒体。
#[test]
fn a_seek_request_is_carried_out_on_the_next_pull() {
    let shared = SyncShared::new();
    let feed = FakeFeed::mono(100);
    let seeks = feed.seeks.clone();
    let mut source = SyncSource::new(feed, shared.clone());
    take(&mut source, 3);

    let verdict = shared.request_seek(Duration::from_millis(50));
    assert_eq!(take(&mut source, 2), values(50, 2));
    assert!(verdict.try_recv().expect("该有裁决").is_ok());
    assert_eq!(*seeks.lock().unwrap(), vec![Duration::from_millis(50)]);
    assert_eq!(shared.position(), Duration::from_millis(51));
}

/// 预定起播：先备到起播位置，起播那一刻之前全是静音，起播那一帧正好是起播位置。
#[test]
fn a_scheduled_start_begins_on_the_exact_frame() {
    let shared = SyncShared::new();
    let mut source = SyncSource::new(FakeFeed::mono(1_000), shared.clone());
    let present = 5_000 * MS;
    shared.set_target(follow(present + 10 * MS, 20 * MS, true, present + 10 * MS));
    shared.block(present, 32);

    let mut expected = vec![0.0; 10];
    expected.extend(values(20, 5));
    assert_eq!(take(&mut source, 15), expected);
}

/// 已经开始的时间线上落后了:往前丢帧追上，追上之前静音，对齐之后照常出声。
#[test]
fn falling_behind_a_running_timeline_skips_ahead_silently() {
    let shared = SyncShared::new();
    let mut source = SyncSource::new(FakeFeed::mono(1_000), shared.clone());
    let present = 5_000 * MS;
    // 该在媒体第 100 帧,手上在第 0 帧
    shared.set_target(follow(present, 100 * MS, true, 0));
    shared.block(present, 5);
    assert_eq!(take(&mut source, 5), vec![0.0; 5], "丢帧之后还在对齐，不出声");

    shared.block(present + 5 * MS, 5);
    assert_eq!(take(&mut source, 3), values(105, 3), "对齐了，照常出声");
}

/// 小误差靠速率修正：超前时读得慢一点，一千帧少读一帧。
#[test]
fn a_small_lead_is_absorbed_by_reading_slower() {
    let shared = SyncShared::new();
    let feed = FakeFeed::mono(10_000);
    let mut source = SyncSource::new(feed, shared.clone());
    let present = 5_000 * MS;
    // 起播就在这一刻，起播位置第 0 帧
    shared.set_target(follow(present, 0, true, present));
    shared.block(present, 1_000);
    take(&mut source, 1_000);
    // 过了一秒，时间线说该在第 998 帧,手上在第 1000 帧:超前 2ms
    shared.set_target(follow(present, -2 * MS, true, present));
    shared.block(present + 1_000 * MS, 1_000);
    take(&mut source, 1_000);

    let at = shared.position().as_secs_f64() * RATE;
    assert!(at < 1_999.5, "该读得慢一点,实际读到第 {at} 帧");
}
