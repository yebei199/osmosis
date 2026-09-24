//! 此刻声音从哪条路由出去：扬声器、蓝牙、有线/USB(#137 ⑤)。
//!
//! 多台一起出声的合同只对「电脑扬声器 + 手机扬声器」做过声学校准;蓝牙、有线的输出延迟没测过，
//! 界面要把这类成员标成「该路由未校准,不保证同步」。查一次要起一个进程(桌面)或走一趟 JNI
//! (安卓),所以结论记五秒。查不出来就是 `None`,界面不替它下结论。

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

static CACHE: Mutex<Option<(Instant, Option<Route>)>> = Mutex::new(None);

/// 此刻的输出路由。
pub fn current() -> Option<Route> {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, route)) = *cache
        && at.elapsed() < REFRESH
    {
        return route;
    }
    let route = detect();
    *cache = Some((Instant::now(), route));
    route
}

/// 桌面：问 PipeWire(经 pipewire-pulse)默认 sink 叫什么。
#[cfg(not(target_os = "android"))]
fn detect() -> Option<Route> {
    let out = std::process::Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| classify_sink(String::from_utf8_lossy(&out.stdout).trim()))
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
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
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
        assert_eq!(classify_sink("bluez_output.AA_BB_CC.1"), Route::Bluetooth);
        assert_eq!(
            classify_sink("alsa_output.usb-Focusrite_Scarlett-00.analog-stereo"),
            Route::Wired
        );
        assert_eq!(
            classify_sink("alsa_output.pci-0000_0c_00.4.analog-stereo"),
            Route::Speaker
        );
    }
}
