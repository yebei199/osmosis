//! 经 rtkit 把送声线程升成实时(#192)。
//!
//! 普通优先级的送声线程要和编译进程平分 CPU,补缓冲晚一次就卡一下。桌面 Linux 上普通进程自己
//! 不能设实时策略，要请 rtkit(D-Bus 系统总线上的 `org.freedesktop.RealtimeKit1`)代办。
//! 不用 cpal 的 `audio_thread_priority` 开关：它经 C 的 libdbus 调 rtkit,发布件就多一个运行期
//! C 库;这里走 zbus,纯 Rust,与 MPRIS 同一条路。
//!
//! 调用都在持流线程上做，实时回调只交出自己的线程号。rtkit 的两条规矩：优先级不超过它的
//! `MaxRealtimePriority`;进程的 `RLIMIT_RTTIME` 软硬限都得设上、且不超过它的 `RTTimeUSecMax`,
//! 否则拒绝。这个限是实时线程一口气能占多久 CPU,超了内核发信号杀进程 —— 送声回调每块都会
//! 睡下去等声卡，远用不到它。

/// 想要的实时优先级。rtkit 默认上限 20,PipeWire 自己的数据线程用的就是 20;
/// 我们比它低一截，免得和服务端抢。
const WANTED_PRIORITY: u32 = 10;

/// 想要的 `RLIMIT_RTTIME`(微秒)。rtkit 默认上限 200ms,就取它。
const WANTED_RTTIME_US: u64 = 200_000;

/// 跟 rtkit 打交道的那几件事。真实现走 D-Bus 与 `setrlimit`,测试换成假的。
pub(super) trait Rtkit {
    fn max_priority(&self) -> Result<i32, String>;
    fn rttime_max_us(&self) -> Result<i64, String>;
    /// 设进程的 `RLIMIT_RTTIME`,软硬限都是 `us`。
    fn limit_rttime(&self, us: u64) -> Result<(), String>;
    fn make_realtime(
        &self,
        tid: u64,
        priority: u32,
    ) -> Result<(), String>;
}

/// 按 rtkit 给的上限夹出这一次请求的优先级与 rttime。上限不是正数就是没法升。
fn clamp(
    max_priority: i32,
    rttime_max_us: i64,
) -> Result<(u32, u64), String> {
    let priority = u32::try_from(max_priority)
        .ok()
        .filter(|&p| p > 0)
        .ok_or_else(|| {
            format!(
                "rtkit 允许的最高优先级是 {max_priority}"
            )
        })?
        .min(WANTED_PRIORITY);
    let rttime = u64::try_from(rttime_max_us)
        .ok()
        .filter(|&us| us > 0)
        .ok_or_else(|| {
            format!("rtkit 允许的 RTTIME 上限是 {rttime_max_us}us")
        })?
        .min(WANTED_RTTIME_US);
    Ok((priority, rttime))
}

/// 把线程 `tid` 升成实时。失败返回原因，由调用方记一行警告 —— 播放照常。
pub(super) fn promote(
    rtkit: &impl Rtkit,
    tid: u64,
) -> Result<(), String> {
    let (priority, rttime) = clamp(
        rtkit.max_priority()?,
        rtkit.rttime_max_us()?,
    )?;
    rtkit.limit_rttime(rttime)?;
    rtkit.make_realtime(tid, priority)
}

/// 系统总线上真的 rtkit。
pub(super) struct SystemRtkit {
    proxy: zbus::blocking::Proxy<'static>,
}

impl SystemRtkit {
    pub(super) fn connect() -> Result<Self, String> {
        let conn = zbus::blocking::Connection::system()
            .map_err(|e| {
                format!("连不上 D-Bus 系统总线: {e}")
            })?;
        let proxy = zbus::blocking::Proxy::new(
            &conn,
            "org.freedesktop.RealtimeKit1",
            "/org/freedesktop/RealtimeKit1",
            "org.freedesktop.RealtimeKit1",
        )
        .map_err(|e| format!("找不到 rtkit: {e}"))?;
        Ok(Self { proxy })
    }
}

impl Rtkit for SystemRtkit {
    fn max_priority(&self) -> Result<i32, String> {
        self.proxy
            .get_property("MaxRealtimePriority")
            .map_err(|e| {
                format!("问不到 rtkit 的最高优先级: {e}")
            })
    }

    fn rttime_max_us(&self) -> Result<i64, String> {
        self.proxy.get_property("RTTimeUSecMax").map_err(
            |e| format!("问不到 rtkit 的 RTTIME 上限: {e}"),
        )
    }

    fn limit_rttime(&self, us: u64) -> Result<(), String> {
        let limit = libc::rlimit {
            rlim_cur: us,
            rlim_max: us,
        };
        // SAFETY: 传进去的是栈上一个完整初始化的 rlimit
        let rc = unsafe {
            libc::setrlimit(libc::RLIMIT_RTTIME, &limit)
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(format!(
                "设不上 RLIMIT_RTTIME: {}",
                std::io::Error::last_os_error()
            ))
        }
    }

    fn make_realtime(
        &self,
        tid: u64,
        priority: u32,
    ) -> Result<(), String> {
        self.proxy
            .call::<_, _, ()>(
                "MakeThreadRealtime",
                &(tid, priority),
            )
            .map_err(|e| format!("rtkit 拒绝升实时: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// 假 rtkit:报给定的上限，记下收到的请求;`refuse` 时拒绝升实时。
    #[derive(Default)]
    struct Fake {
        max_priority: i32,
        rttime_max_us: i64,
        refuse: bool,
        calls: RefCell<Vec<String>>,
    }

    impl Rtkit for Fake {
        fn max_priority(&self) -> Result<i32, String> {
            Ok(self.max_priority)
        }
        fn rttime_max_us(&self) -> Result<i64, String> {
            Ok(self.rttime_max_us)
        }
        fn limit_rttime(
            &self,
            us: u64,
        ) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("rttime {us}"));
            Ok(())
        }
        fn make_realtime(
            &self,
            tid: u64,
            priority: u32,
        ) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("rt {tid} {priority}"));
            if self.refuse {
                Err("拒绝".to_owned())
            } else {
                Ok(())
            }
        }
    }

    /// 上限宽裕：要我们想要的;上限更紧：照上限来。RTTIME 先设、再请求升级(rtkit 先查它)。
    #[test]
    fn requests_stay_within_what_rtkit_allows() {
        let roomy = Fake {
            max_priority: 20,
            rttime_max_us: 1_000_000,
            ..Fake::default()
        };
        promote(&roomy, 42).unwrap();
        assert_eq!(
            *roomy.calls.borrow(),
            vec!["rttime 200000", "rt 42 10"]
        );

        let tight = Fake {
            max_priority: 5,
            rttime_max_us: 50_000,
            ..Fake::default()
        };
        promote(&tight, 42).unwrap();
        assert_eq!(
            *tight.calls.borrow(),
            vec!["rttime 50000", "rt 42 5"]
        );
    }

    /// 上限不是正数、或者 rtkit 拒绝：交回原因，不 panic。上限不对时根本不去请求。
    #[test]
    fn a_refusal_is_reported_not_raised() {
        let none = Fake::default();
        assert!(promote(&none, 42).is_err());
        assert!(none.calls.borrow().is_empty());

        let refusing = Fake {
            max_priority: 20,
            rttime_max_us: 200_000,
            refuse: true,
            ..Fake::default()
        };
        let reason = promote(&refusing, 42).unwrap_err();
        assert!(reason.contains("拒绝"), "{reason}");
    }
}
