//! 满载回归检查的被测进程(#192):照生产的路子放一路正弦波，数送声卡时报的欠载。
//!
//! 走的是真 `Player`:同步源、`buffered` 的解码线程、cpal 流，都和放歌时同一条路。
//! 欠载从 cpal 的错误回调来 —— 输出层把它记成一行 `cpal 输出出错` 的警告，这里装一个
//! 只数这行的 logger,不往 `Player` 上为测试开口子。
//!
//! 用法:`load_probe <秒数>`。每秒往 stdout 打一行 `xruns=<累计>`,结束时打
//! `done xruns=<累计>`。出到哪个设备由环境决定(`test/audio-rt-load.sh` 用
//! `PIPEWIRE_NODE` 指到它临时建的 null sink)。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rodio::source::SineWave;

/// 输出层报欠载时那行警告的开头(见 `src/output/cpal.rs` 的错误回调)。
const XRUN_PREFIX: &str = "cpal 输出出错";

static XRUNS: AtomicU64 = AtomicU64::new(0);

struct XrunCounter;

impl log::Log for XrunCounter {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let line = record.args().to_string();
        if line.starts_with(XRUN_PREFIX) {
            XRUNS.fetch_add(1, Ordering::Relaxed);
        }
        if record.level() <= log::Level::Warn {
            eprintln!("{}: {line}", record.level());
        }
    }

    fn flush(&self) {}
}

static LOGGER: XrunCounter = XrunCounter;

fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    log::set_logger(&LOGGER).expect("logger 只装这一次");
    log::set_max_level(log::LevelFilter::Info);

    let player =
        audio::Player::new().expect("开不了输出设备");
    let tone = audio::pcm::normalize(SineWave::new(440.0));
    player.play_feed(audio::buffered(tone));

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_secs(1));
        println!("xruns={}", XRUNS.load(Ordering::Relaxed));
    }
    println!(
        "done xruns={}",
        XRUNS.load(Ordering::Relaxed)
    );
}
