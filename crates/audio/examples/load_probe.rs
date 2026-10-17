//! 满载回归检查的被测进程(#192):照生产的路子放一路正弦波，报送声卡时的欠载与旁证。
//!
//! 走的是真 `Player`:同步源、`buffered` 的解码线程、cpal 流，都和放歌时同一条路。所有数都取自
//! `Player::sync_report` 与 `Player::sync_events` —— 回调那一侧只写原子量，这里每秒读一次。
//!
//! 用法:`load_probe <秒数>`。每秒打一行累计数，结束时打 `done xruns=<累计>`,再按时间顺序
//! 打出回调记下的事件(超时的块带墙钟与 CPU 时间、欠载的时刻),时刻从起播算起。
//! 出到哪个设备由环境决定(`test/audio-rt-load.sh` 用 `PIPEWIRE_NODE` 指到它临时建的 null sink)。

use std::time::{Duration, Instant};

use audio::sync::Event;
use rodio::source::SineWave;

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let player =
        audio::Player::new().expect("开不了输出设备");
    let tone = audio::pcm::normalize(SineWave::new(440.0));
    let start_ns = audio::clock::monotonic_ns();
    player.play_feed(audio::buffered(tone));

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_secs(1));
        let r = player.sync_report();
        println!(
            "xruns={} other_errors={} starves={} fill_misses={} worst_ms={:.2} \
             worst_cpu_ms={:.2} late={} gap_worst_ms={:.2} queue_low_frames={} \
             period_frames={} feed_low={}",
            r.underruns,
            r.other_errors,
            r.starves,
            r.fill_misses,
            ms(r.callback_worst_ns),
            ms(r.callback_worst_cpu_ns),
            r.callbacks_late,
            ms(r.wake_gap_worst_ns),
            r.queue_low_frames,
            r.period_frames,
            r.feed_low,
        );
    }
    println!(
        "done xruns={}",
        player.sync_report().underruns
    );
    let at = |ns: i64| (ns - start_ns) as f64 / 1e9;
    for event in player.sync_events() {
        match event {
            Event::Late {
                at_ns,
                wall_ns,
                cpu_ns,
            } => println!(
                "event late at={:.3} wall_ms={:.2} cpu_ms={:.2}",
                at(at_ns),
                ms(wall_ns),
                ms(cpu_ns)
            ),
            Event::Underrun { at_ns } => {
                println!("event xrun at={:.3}", at(at_ns));
            }
        }
    }
}
