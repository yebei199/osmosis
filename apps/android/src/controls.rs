//! Android 这一端的系统媒体控件:把状态送进 Java,把按键接回来。
//!
//! 锁屏与通知栏那条播放条是 **SystemUI 画的**,不是我们画的 —— 应用画什么都只在
//! 自己的窗口里。系统愿意替你画,唯一的条件是注册一个 `MediaSession` 并挂一条
//! `MediaStyle` 通知,那两样都是 Java 的东西,住在
//! `gradle/app/src/main/java/io/github/osmosis/`(理由见 `docs/adr/0020`)。
//!
//! 这个模块就是那两侧之间的桥,不记任何状态。

use std::sync::{Arc, OnceLock};

use jni::JavaVM;
use jni::sys::{jint, jlong};

/// 命令的出口。
///
/// JNI 的原生方法是个裸符号,调进来时拿不到任何上下文,所以这条通道只能是全局的。
/// `OnceLock` 而不是 `Mutex`:它只在 `start` 里写一次,之后一直只读。
static COMMAND: OnceLock<
    Arc<dyn Fn(ui::MediaCommand) + Send + Sync>,
> = OnceLock::new();

/// 接上 Java 那一侧。接不上就退回什么都不做 —— 没有媒体控件不影响出声。
pub fn start(
    app: &slint::android::AndroidApp,
    hooks: ui::MediaHooks,
) -> Box<dyn ui::MediaControls> {
    // SAFETY:`vm_as_ptr` 返回的是 android-activity 在 `android_main` 之前就拿到
    // 的那个 JavaVM 指针,进程存续期间一直有效。
    let vm =
        unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };

    if COMMAND.set(hooks.command.clone()).is_err() {
        log::warn!("媒体控件被接了第二次,后一次没有生效");
    }

    Box::new(Controls {
        vm,
        position: hooks.position.clone(),
    })
}

struct Controls {
    vm: JavaVM,
    /// 位置每次推送现问一次。安卓 13+ 的通知会按倍率自己往前推,所以推一次
    /// 准确的就够,不必每秒推。
    position:
        Arc<dyn Fn() -> core::time::Duration + Send + Sync>,
}

impl ui::MediaControls for Controls {
    fn publish(&self, now: &ui::NowPlaying) {
        if let Err(err) = self.push(now) {
            // 推不出去只是控件不更新,不该影响出声。
            log::warn!("媒体控件推送失败: {err}");
        }
    }
}

impl Controls {
    fn push(
        &self,
        now: &ui::NowPlaying,
    ) -> jni::errors::Result<()> {
        let (argb, width, height) = argb_pixels(now);

        self.vm.attach_current_thread(|env| {
            let title = env.new_string(&now.title)?;
            // 通知上只有一行的位置,拼接是这一端的选择(界面那边另有自己的拼法)。
            let artists =
                env.new_string(now.artists.join(" / "))?;

            let pixels = env.new_int_array(argb.len())?;
            if !argb.is_empty() {
                pixels.set_region(env, 0, &argb)?;
            }

            env.call_static_method(
                // 与 `MediaControls.java` 的包名 + 类名绑死。
                jni::jni_str!(
                    "io/github/osmosis/MediaControls"
                ),
                jni::jni_str!("publish"),
                // 参数顺序与 `MediaControls.publish` 的形参一一对应,改一边就要改
                // 另一边 —— 对不上抛的是 NoSuchMethodError,而且要等到第一次
                // 换歌才抛。
                jni::jni_sig!(
                    "(ILjava/lang/String;Ljava/lang/String;JJ[III)V"
                ),
                &[
                    jni::objects::JValue::Int(status_code(
                        now.status,
                    )),
                    (&title).into(),
                    (&artists).into(),
                    jni::objects::JValue::Long(
                        now.duration_ms,
                    ),
                    jni::objects::JValue::Long(
                        (self.position)().as_millis() as i64,
                    ),
                    (&pixels).into(),
                    jni::objects::JValue::Int(width),
                    jni::objects::JValue::Int(height),
                ],
            )?;
            Ok(())
        })
    }
}

/// 与 `MediaControls.java` 的 `STATUS_*` 常量一一对应。
fn status_code(status: ui::MediaStatus) -> jint {
    match status {
        ui::MediaStatus::Playing => 0,
        ui::MediaStatus::Paused => 1,
        ui::MediaStatus::Stopped => 2,
    }
}

// 随机与循环不再过这条 seam:它们从通知栏撤掉了(见 `MediaControlsService.java`),
// 安卓这一端没有别的地方显示或改动它们。`ui::NowPlaying` 上那两个字段留着 ——
// 桌面 MPRIS 要用。

/// 封面像素从 RGBA 重排成 `Bitmap.Config.ARGB_8888` 要的那种打包 int。
///
/// **这一步错了不会黑屏,只会颜色不对**,所以别看到"有图"就以为对了:红蓝互换
/// 出来的图仍然是一张像模像样的图。
///
/// 安卓那个 int 是按机器字序存的 `0xAARRGGBB`,我们手上是逐字节的 R、G、B、A。
fn argb_pixels(
    now: &ui::NowPlaying,
) -> (Vec<jint>, jint, jint) {
    let Some(art) = now.art.as_ref() else {
        return (Vec::new(), 0, 0);
    };

    let expected =
        art.width as usize * art.height as usize * 4;
    if art.rgba.len() != expected {
        // 尺寸对不上就别送 —— Java 那边会按宽高去索引,越界要么抛要么花屏。
        log::warn!(
            "封面像素数不对: {} != {expected}",
            art.rgba.len()
        );
        return (Vec::new(), 0, 0);
    }

    let packed = art
        .rgba
        .chunks_exact(4)
        .map(|px| {
            i32::from_be_bytes([px[3], px[0], px[1], px[2]])
        })
        .collect();

    (packed, art.width as jint, art.height as jint)
}

/// Java 那一侧按下的键。
///
/// 符号名与 `MediaControls.java` 的包名、类名、方法名绑死 —— 改任何一个都要同时
/// 改这里。**链接期不会有人提醒**,只会在按下按钮那一刻抛 `UnsatisfiedLinkError`
/// (那边为此留了一个 catch,免得把通知栏一起带崩)。
///
/// 拿裸的 sys 类型而不是 `Env`:这个函数根本不碰 JNI,只是把参数翻成
/// [`ui::MediaCommand`] 交给闭包。
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_osmosis_MediaControls_nativeCommand(
    _env: *mut jni::sys::JNIEnv,
    _class: jni::sys::jclass,
    command: jint,
    argument: jlong,
) {
    // panic 不能穿过 FFI 边界。这里面只有一次闭包调用,但那个闭包会一路走到
    // Slint 的事件循环上去。
    let caught = std::panic::catch_unwind(|| {
        let Some(command) = decode(command, argument)
        else {
            log::warn!(
                "媒体控件送来一个不认识的键: {command}"
            );
            return;
        };
        let Some(sink) = COMMAND.get() else {
            log::warn!("媒体控件还没接上,这一下被丢掉了");
            return;
        };
        sink(command);
    });

    if caught.is_err() {
        log::error!("媒体控件的按键处理 panic 了");
    }
}

/// 与 `MediaControls.java` 的 `COMMAND_*` 常量一一对应。
fn decode(
    command: jint,
    argument: jlong,
) -> Option<ui::MediaCommand> {
    Some(match command {
        0 => ui::MediaCommand::Play,
        1 => ui::MediaCommand::Pause,
        2 => ui::MediaCommand::Toggle,
        3 => ui::MediaCommand::Next,
        4 => ui::MediaCommand::Previous,
        5 => ui::MediaCommand::SeekTo(argument),
        6 => ui::MediaCommand::SeekBy(argument),
        // 参数是绝对值而不是「翻一下」:让 Java 侧去猜「现在是不是随机」,
        // 它记的那一份迟早会跟队列对不上。
        7 => ui::MediaCommand::SetShuffle(argument != 0),
        // 循环同理,参数是要拨到的绝对态。
        8 => ui::MediaCommand::SetLoop(match argument {
            1 => ui::LoopMode::All,
            2 => ui::LoopMode::One,
            _ => ui::LoopMode::Off,
        }),
        _ => return None,
    })
}
