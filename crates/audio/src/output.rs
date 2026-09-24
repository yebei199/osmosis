//! 输出后端的门面：开一条声卡流，从混音器拉采样，每块把呈现时刻报给同步层。
//! 见 `output/README.md`。

use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use rodio::mixer::{Mixer, MixerSource};

use crate::AudioError;
use crate::sync::SyncShared;

#[cfg(target_os = "android")]
mod aaudio;
#[cfg(target_os = "android")]
use aaudio as backend;

pub mod route;

#[cfg(not(target_os = "android"))]
mod cpal;
#[cfg(not(target_os = "android"))]
use self::cpal as backend;

/// 持流的那条线程多久醒一次，看流是不是断了要重开。
const WATCH_EVERY: std::time::Duration =
    std::time::Duration::from_millis(250);

/// 一条活着的输出。drop 掉就关流、收线程。
pub(crate) struct Output {
    pub mixer: Mixer,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 回调从这里拉采样。同一时刻只有一条流在用它;重开时新流接着拉同一个混音器。
pub(crate) type SharedMixer = Arc<Mutex<MixerSource>>;

/// 把混音器的采样填进声卡的缓冲。锁拿不到(不该发生：同一时刻只有一条流)就填静音。
pub(crate) fn fill(source: &SharedMixer, out: &mut [f32]) {
    match source.try_lock() {
        Ok(mut source) => {
            for sample in out.iter_mut() {
                *sample = source.next().unwrap_or(0.0);
            }
        }
        Err(_) => out.fill(0.0),
    }
}

/// 开默认输出设备。流关在一条专属线程里：流对象不跨线程，断了也在那条线程上重开。
pub(crate) fn open(
    shared: Arc<SyncShared>,
) -> Result<Output, AudioError> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("audio-output".to_owned())
        .spawn(move || {
            backend::run(shared, &ready_tx, &stop_rx)
        })
        .map_err(|e| AudioError::Device(e.to_string()))?;
    let mixer = ready_rx
        .recv()
        .map_err(|_| {
            AudioError::Device(
                "输出线程没开出设备就退出了".to_owned(),
            )
        })??;
    Ok(Output {
        mixer,
        stop: stop_tx,
        thread: Some(thread),
    })
}

/// 持流线程的等待循环：收到关闭就走，流断了就交给 `reopen` 重开。
pub(crate) fn watch(
    stop: &mpsc::Receiver<()>,
    broken: &dyn Fn() -> bool,
    mut reopen: impl FnMut(),
) {
    loop {
        match stop.recv_timeout(WATCH_EVERY) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if broken() {
                    reopen();
                }
            }
        }
    }
}
