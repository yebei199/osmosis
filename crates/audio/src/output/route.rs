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

/// 快照多老就不再发布。正常情况下快照最老是 `REFRESH` 加一次探测(桌面两条命令各
/// `COMMAND_BUDGET` 加杀后的回收等待，约四秒);
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
    let before = CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .replace((Instant::now(), route));
    // 变了才记一行：扬声器在界面上不标注，路由探测有没有在干活只能从日志看。放了锁再写日志，
    // 日志卡住也卡不到界面那一侧
    if before.is_none_or(|(_, before)| before != route) {
        log::info!("输出路由: {route:?}");
    }
}

/// 桌面：问 PipeWire 默认 sink 叫什么。
#[cfg(not(target_os = "android"))]
fn detect() -> Option<Route> {
    detect_with(&|program, args| {
        bounded::run(program, args, COMMAND_BUDGET)
    })
}

/// 限时跑外部命令。探测线程在这里的每一处等待都有上限：子进程杀不死时交给后台回收，
/// 探测线程照样收场，下一轮还起得来。
#[cfg(not(target_os = "android"))]
mod bounded {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// 两次查子进程退没退之间睡多久。比命令正常的应答时间短，又不至于空转。
    const POLL: Duration = Duration::from_millis(10);

    /// SIGKILL 之后等它退多久。正常几毫秒就退;卡在内核里(D 状态)的进程信号要等系统调用返回
    /// 才生效，等不起。
    const REAP_GRACE: Duration = Duration::from_millis(500);

    /// 进程退了以后，stdout 最少再等多久读完。
    const READ_GRACE: Duration = Duration::from_millis(100);

    /// 还没收场的后台活：杀了还没退、交给后台回收的子进程，加上还没读到 EOF 的读线程。
    /// 不为零就不再起新命令 —— 卡住的进程或读线程一轮攒一个，不如这段时间直接报未知。
    static LINGERING: AtomicUsize = AtomicUsize::new(0);

    /// 跑一条命令，最多等 `budget`;成功时交回 stdout。超时就连同它起的子孙(同一进程组)一起杀掉并回收。
    /// stdout 由读线程收，输出写满管道也不会把子进程卡住;读线程等不到 EOF(子孙还攥着写端)
    /// 时同样杀整组。读线程算进 `LINGERING`,真有子孙逃出进程组、一直攥着写端，也只卡住这一个。
    pub(super) fn run(
        program: &str,
        args: &[&str],
        budget: Duration,
    ) -> Option<String> {
        let (mut child, stdout) =
            read_stdout(start(program, args)?)?;
        let deadline = Instant::now() + budget;
        let Some(status) = wait_until(&mut child, deadline)
        else {
            log::warn!("{program} {budget:?} 内没答，杀掉");
            abandon(child, Some(&stdout));
            return None;
        };
        let out =
            collect(program, &child, &stdout, deadline)?;
        status.success().then(|| {
            String::from_utf8_lossy(&out).into_owned()
        })
    }

    /// 起子进程，自成一个进程组;还有没收场的后台活就不起。
    fn start(
        program: &str,
        args: &[&str],
    ) -> Option<Child> {
        if LINGERING.load(Ordering::Acquire) > 0 {
            return None;
        }
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok()
    }

    /// 读线程收到的 stdout。
    type Stdout = mpsc::Receiver<std::io::Result<Vec<u8>>>;

    /// 起读线程收 stdout。起不来就放弃子进程。
    fn read_stdout(
        mut child: Child,
    ) -> Option<(Child, Stdout)> {
        let Some(mut pipe) = child.stdout.take() else {
            abandon(child, None);
            return None;
        };
        let (sender, receiver) = mpsc::channel();
        LINGERING.fetch_add(1, Ordering::AcqRel);
        let reader = std::thread::Builder::new()
            .name("route-read".into())
            .spawn(move || {
                let mut out = Vec::new();
                let read =
                    pipe.read_to_end(&mut out).map(|_| out);
                // 先减计数再交结果：收到结果的一方据此确知读线程已收场
                LINGERING.fetch_sub(1, Ordering::AcqRel);
                let _ = sender.send(read);
            });
        if reader.is_err() {
            LINGERING.fetch_sub(1, Ordering::AcqRel);
            abandon(child, None);
            return None;
        }
        Some((child, receiver))
    }

    /// 进程退了，在剩下的预算里(至少 `READ_GRACE`)收 stdout。收不到说明子孙还攥着写端，杀整组。
    fn collect(
        program: &str,
        child: &Child,
        stdout: &Stdout,
        deadline: Instant,
    ) -> Option<Vec<u8>> {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .max(READ_GRACE);
        let read = stdout.recv_timeout(left).ok();
        if read.is_none() {
            log::warn!(
                "{program} 退了但 stdout 没读完，杀掉它留下的进程"
            );
            kill_group(child);
        }
        read?.ok()
    }

    /// 放弃一个没答的子进程：杀整组、回收，再给读线程一点时间收场 —— 写端随进程关掉，
    /// 它马上就读到 EOF;不等的话下一条命令会被 `LINGERING` 挡掉。
    fn abandon(child: Child, stdout: Option<&Stdout>) {
        kill_group(&child);
        reap(child);
        if let Some(stdout) = stdout {
            let _ = stdout.recv_timeout(READ_GRACE);
        }
    }

    /// 等子进程退，最晚到 `deadline`。没退或查不了就是 `None`。
    fn wait_until(
        child: &mut Child,
        deadline: Instant,
    ) -> Option<std::process::ExitStatus> {
        loop {
            let status = child
                .try_wait()
                .inspect_err(|error| {
                    log::warn!("等子进程失败: {error}");
                })
                .ok()?;
            if status.is_some()
                || Instant::now() >= deadline
            {
                return status;
            }
            std::thread::sleep(POLL);
        }
    }

    /// 杀子进程所在的整个进程组(`process_group(0)` 让组号等于它的 pid)。
    fn kill_group(child: &Child) {
        let Ok(group) = libc::pid_t::try_from(child.id())
        else {
            return;
        };
        // SAFETY:kill 只读两个整数参数，不碰内存。组号是我们起的子进程的 pid,组里还有进程时
        // 内核不会把它分给别人;组里没人了，这一枪打空。
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }

    /// 回收杀掉的子进程，不留僵尸。`REAP_GRACE` 内没退就交给后台线程去等，探测线程不陪它卡着。
    fn reap(mut child: Child) {
        let id = child.id();
        if wait_until(
            &mut child,
            Instant::now() + REAP_GRACE,
        )
        .is_some()
        {
            return;
        }
        log::warn!(
            "子进程 {id} 杀了 {REAP_GRACE:?} 还没回收，交给后台"
        );
        LINGERING.fetch_add(1, Ordering::AcqRel);
        let spawned = std::thread::Builder::new()
            .name("route-reap".into())
            .spawn(move || {
                let _ = child.wait();
                LINGERING.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            // 线程都起不来，就让计数留着：之后报未知，不再起新进程
            log::warn!("起不了回收线程，路由探测停用");
        }
    }
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
