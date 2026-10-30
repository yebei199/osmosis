//! 此刻声音从哪条路由出去：扬声器、蓝牙、有线/USB(#137 ⑤)。
//!
//! 多台一起出声的合同只对「电脑扬声器 + 手机扬声器」做过声学校准;蓝牙、有线的输出延迟没测过，
//! 界面要把这类成员标成「该路由未校准,不保证同步」。查一次要起一两个进程(桌面)或走一趟 JNI
//! (安卓),所以结论记五秒。查不出来就是 `None`,界面不替它下结论。
//!
//! 调用方是界面每秒一次的定时回调，所以 `current` 从不等探测：它只读快照，快照到期就在后台起一次
//! 探测，这一轮照旧交回快照。MEO-2848:蓝牙通道出错时 `wpctl inspect` 卡了几分钟，同步探测把整个
//! 回调连同进度、续播、媒体状态一起堵死。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 输出路由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Speaker,
    Bluetooth,
    Wired,
}

/// 结论记多久。插拔耳机、连蓝牙之后最迟这么久界面就跟上。
const REFRESH: Duration = Duration::from_secs(5);

/// 单条命令最多等多久。正常的 `wpctl inspect` / `pactl get-default-sink` 几十毫秒就答;
/// 两条加起来不到 `REFRESH`,一次探测在下一次该起之前就收场。
#[cfg_attr(target_os = "android", allow(dead_code))]
const COMMAND_BUDGET: Duration =
    Duration::from_millis(1500);

/// 快照多老就不再发布。正常情况下快照最老是 `REFRESH` 加一次探测(桌面最多两个 `COMMAND_BUDGET`);
/// 超过这个数说明探测线程起不来，或卡在没有超时的地方(安卓的 JNI),这时报未知，不拿几分钟前的
/// 设备冒充现在。
const STALE: Duration = Duration::from_secs(12);

static CACHE: Mutex<Option<(Instant, Option<Route>)>> =
    Mutex::new(None);

/// 有一次探测在跑。并发的刷新请求都并进这一次，同一时刻最多一个探测。
static PROBING: AtomicBool = AtomicBool::new(false);

/// 此刻的输出路由。不阻塞：锁只在读写快照时拿一下，探测在后台线程里跑。
pub fn current() -> Option<Route> {
    let snapshot = *CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if snapshot
        .is_none_or(|(at, _)| at.elapsed() >= REFRESH)
    {
        spawn_probe();
    }
    match snapshot {
        Some((at, route)) if at.elapsed() < STALE => route,
        _ => None,
    }
}

/// 后台探一次，结论写回快照。已有一次在跑就什么也不做。
fn spawn_probe() {
    if PROBING
        .compare_exchange(
            false,
            true,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("route-probe".into())
        .spawn(probe);
    if let Err(error) = spawned {
        log::warn!("起不了路由探测线程: {error}");
        PROBING.store(false, Ordering::Release);
    }
}

/// 探测线程的身子：探一次、写快照、放开占位。
fn probe() {
    // 线程收场(包括 panic)时放开占位，下一轮才起得了新的
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            PROBING.store(false, Ordering::Release);
        }
    }
    let _release = Release;
    let route = detect();
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    // 变了才记一行：扬声器在界面上不标注，路由探测有没有在干活只能从日志看
    if cache.is_none_or(|(_, before)| before != route) {
        log::info!("输出路由: {route:?}");
    }
    *cache = Some((Instant::now(), route));
}

/// 桌面：问 PipeWire 默认 sink 叫什么。
#[cfg(not(target_os = "android"))]
fn detect() -> Option<Route> {
    detect_with(&|program, args| {
        run_bounded(program, args, COMMAND_BUDGET)
    })
}

/// 跑一条命令，最多等 `budget`;成功时交回 stdout。超时就杀掉并 `wait` 回收，不留僵尸、不攒进程。
/// stdout 由读线程收，输出写满管道也不会把子进程卡住;子进程一死写端就关，读线程跟着收场。
#[cfg(not(target_os = "android"))]
fn run_bounded(
    program: &str,
    args: &[&str],
    budget: Duration,
) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    /// 两次查子进程退没退之间睡多久。比命令正常的应答时间短，又不至于空转。
    const POLL: Duration = Duration::from_millis(10);

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });
    let deadline = Instant::now() + budget;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(POLL);
            }
            Ok(None) => {
                log::warn!(
                    "{program} {budget:?} 内没答，杀掉"
                );
                break None;
            }
            Err(error) => {
                log::warn!("等 {program} 失败: {error}");
                break None;
            }
        }
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let out = reader.join().ok()?.ok()?;
    status?
        .success()
        .then(|| String::from_utf8_lossy(&out).into_owned())
}

/// 先问 `wpctl`(WirePlumber,纯 PipeWire 桌面必有),再问 `pactl`(pipewire-pulse);两个都不在
/// 或都答不出 sink 名，就是 `None`。`run` 跑一条命令，成功时交回 stdout。
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn detect_with(
    run: &dyn Fn(&str, &[&str]) -> Option<String>,
) -> Option<Route> {
    let wpctl =
        run("wpctl", &["inspect", "@DEFAULT_AUDIO_SINK@"])
            .and_then(|out| {
                wpctl_node_name(&out).map(str::to_owned)
            });
    let name = wpctl.or_else(|| {
        run("pactl", &["get-default-sink"])
            .map(|out| out.trim().to_owned())
            .filter(|name| !name.is_empty())
    })?;
    Some(classify_sink(&name))
}

/// 从 `wpctl inspect` 的输出里取 `node.name`。
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn wpctl_node_name(
    inspect: &str,
) -> Option<&str> {
    inspect.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim_start_matches([' ', '*']).trim()
            == "node.name")
            .then(|| value.trim().trim_matches('"'))
            .filter(|name| !name.is_empty())
    })
}

/// 从 sink 名认路由：`bluez_output.*` 是蓝牙，名字里带 `usb` 的是 USB 声卡;板载模拟口与 HDMI
/// 算扬声器 —— 模拟口插的是耳机还是音箱，名字上分不出来，按合同校准过的那一种算。
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn classify_sink(name: &str) -> Route {
    if name.starts_with("bluez") {
        Route::Bluetooth
    } else if name.contains("usb") {
        Route::Wired
    } else {
        Route::Speaker
    }
}

/// 安卓：问 `AudioManager` 蓝牙 A2DP、有线耳机开着没有。
#[cfg(target_os = "android")]
fn detect() -> Option<Route> {
    use jni::objects::JObject;

    let context = ndk_context::android_context();
    // SAFETY:android-activity 在进程起来时把 JavaVM 登记进 ndk-context,进程存续期间一直有效。
    let vm = unsafe {
        jni::JavaVM::from_raw(context.vm().cast())
    };
    vm.attach_current_thread(|env| -> jni::errors::Result<Route> {
        // SAFETY:同上登记的 Activity 全局引用，活得比这次调用长;`JObject` drop 时不删它。
        let activity = unsafe { JObject::from_raw(env, context.context().cast()) };
        let service = env.new_string("audio")?;
        let manager = env
            .call_method(
                &activity,
                jni::jni_str!("getSystemService"),
                jni::jni_sig!("(Ljava/lang/String;)Ljava/lang/Object;"),
                &[(&service).into()],
            )?
            .l()?;
        let bluetooth = env
            .call_method(
                &manager,
                jni::jni_str!("isBluetoothA2dpOn"),
                jni::jni_sig!("()Z"),
                &[],
            )?
            .z()?;
        let wired = env
            .call_method(
                &manager,
                jni::jni_str!("isWiredHeadsetOn"),
                jni::jni_sig!("()Z"),
                &[],
            )?
            .z()?;
        Ok(if bluetooth {
            Route::Bluetooth
        } else if wired {
            Route::Wired
        } else {
            Route::Speaker
        })
    })
    .inspect_err(|error| log::warn!("查输出路由失败: {error}"))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 蓝牙、USB 各认得出;板载口按扬声器算。
    #[test]
    fn sink_names_map_to_routes() {
        assert_eq!(
            classify_sink("bluez_output.AA_BB_CC.1"),
            Route::Bluetooth
        );
        assert_eq!(
            classify_sink(
                "alsa_output.usb-Focusrite_Scarlett-00.analog-stereo"
            ),
            Route::Wired
        );
        assert_eq!(
            classify_sink(
                "alsa_output.pci-0000_0c_00.4.analog-stereo"
            ),
            Route::Speaker
        );
    }
    /// `wpctl inspect` 的 node.name 行带不带 `*` 前缀都认得出;没有这一行就是 `None`。
    #[test]
    fn wpctl_inspect_yields_node_name() {
        let inspect = "id 56, type PipeWire:Interface:Node\n    \
            device.bus = \"pci\"\n  * node.name = \"alsa_output.pci-0000_07_00.6.analog-stereo\"\n    \
            node.nick = \"ALCS1200A Analog\"\n";
        assert_eq!(
            wpctl_node_name(inspect),
            Some(
                "alsa_output.pci-0000_07_00.6.analog-stereo"
            )
        );
        assert_eq!(
            wpctl_node_name(
                "    node.name = \"bluez_output.AA_BB.1\"\n"
            ),
            Some("bluez_output.AA_BB.1")
        );
        assert_eq!(
            wpctl_node_name(
                "id 56, type PipeWire:Interface:Node\n"
            ),
            None
        );
        assert_eq!(
            wpctl_node_name("    node.name = \"\"\n"),
            None
        );
    }

    /// 有 wpctl 就用它;没有退到 pactl;两个都没有报未知，不误标成扬声器。
    #[test]
    fn detect_prefers_wpctl_then_pactl_then_unknown() {
        let wpctl_only = |program: &str, _: &[&str]| {
            (program == "wpctl").then(|| {
                "  * node.name = \"bluez_output.AA.1\"\n".to_owned()
            })
        };
        assert_eq!(
            detect_with(&wpctl_only),
            Some(Route::Bluetooth)
        );

        let pactl_only = |program: &str, _: &[&str]| {
            (program == "pactl").then(|| {
                "alsa_output.usb-Focusrite-00.analog-stereo\n".to_owned()
            })
        };
        assert_eq!(
            detect_with(&pactl_only),
            Some(Route::Wired)
        );

        let neither = |_: &str, _: &[&str]| None;
        assert_eq!(detect_with(&neither), None);

        // wpctl 在但答不出 node.name(默认 sink 不存在):不拿空名去猜
        let wpctl_blank = |program: &str, _: &[&str]| {
            (program == "wpctl").then(String::new)
        };
        assert_eq!(detect_with(&wpctl_blank), None);
    }
}
