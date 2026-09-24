//! 应用内升级的 Rust 侧:把核对过的 APK 交给 Java 那半边(`gradle/.../Updater.java`)。
//!
//! 下载与校验在 `api::update`,状态与文案在 `ui::update`,这里只是一座桥,
//! 与 `downloads.rs` 同一个形状。
//!
//! 与另外两座桥有一处不同:安装跑在 tokio 的阻塞池上(要把一百多 MB 拷进安装会话),
//! 而那种线程是 native 起的,挂的是**系统**类加载器 —— 在那里按名字找
//! `io/github/osmosis/Updater` 必然找不到(#129 真机:「failed to resolve Java class」)。
//! 媒体控件与下载的调用都落在 `android_main` 线程上,android-activity 给那条线程设好了
//! 应用的类加载器,所以它们没事。这里在 `start`(也在 `android_main` 上)把类加载好、
//! 存成全局引用,之后在哪条线程上都拿它直接调。

use std::path::Path;
use std::sync::OnceLock;

use jni::errors::LogErrorAndDefault;
use jni::objects::{
    JClass, JObject, JString, LoaderContext,
};
use jni::refs::Global;
use jni::sys::jint;
use jni::{EnvUnowned, JavaVM};

/// `Updater` 这个类本身。在 `android_main` 线程上解析好,供任意线程使用。
static CLASS: OnceLock<Global<JClass<'static>>> =
    OnceLock::new();

/// JavaVM 的裸指针。理由同 `downloads.rs` 的 `VM`。
static VM: OnceLock<usize> = OnceLock::new();

/// 接上安装器。APK 落在应用私有目录的 `update/` 下,不需要任何存储权限。
///
/// 必须在 `android_main` 线程上调(类要在这里解析,见模块说明)。
pub fn start(app: &slint::android::AndroidApp) {
    let Some(dir) = app.internal_data_path() else {
        log::warn!("拿不到应用私有目录,应用内升级不可用");
        return;
    };
    let class = match resolve_class(app) {
        Ok(class) => class,
        Err(err) => {
            log::warn!(
                "找不到安装器的 Java 类,应用内升级不可用: {err}"
            );
            return;
        }
    };
    if CLASS.set(class).is_err()
        || VM.set(app.vm_as_ptr() as usize).is_err()
    {
        log::warn!("安装器被接了第二次,后一次没有生效");
        return;
    }
    ui::install_updater(dir.join("update"), install);
}

/// 用应用的类加载器解析 `Updater`:先看当前线程的上下文加载器,再看 Activity 所属类
/// 的加载器(那就是应用的加载器),最后才退回 `FindClass`。
fn resolve_class(
    app: &slint::android::AndroidApp,
) -> jni::errors::Result<Global<JClass<'static>>> {
    vm(app.vm_as_ptr() as usize).attach_current_thread(
        |env| {
            // SAFETY:`activity_as_ptr` 是 android-activity 持有的那个 Activity 全局引用,
            // 活得比这次调用长;`JObject` 不会在 drop 时删它。
            let activity = unsafe {
                JObject::from_raw(
                    env,
                    app.activity_as_ptr().cast(),
                )
            };
            let class =
                LoaderContext::FromObject(&activity)
                    .load_class(
                        env,
                        jni::jni_str!(
                            "io.github.osmosis.Updater"
                        ),
                        false,
                    )?;
            env.new_global_ref(class)
        },
    )
}

fn install(apk: &Path) -> Result<(), String> {
    let path =
        apk.to_str().ok_or("安装包路径不是 UTF-8")?;
    let class = CLASS.get().ok_or("安装器还没接上")?;
    let ptr = *VM.get().ok_or("安装器还没接上")?;
    let handed = vm(ptr)
        .attach_current_thread(|env| {
            let path = env.new_string(path)?;
            env.call_static_method(
                class,
                jni::jni_str!("install"),
                jni::jni_sig!("(Ljava/lang/String;)Z"),
                &[(&path).into()],
            )?
            .z()
        })
        .map_err(|e| e.to_string())?;
    if handed {
        Ok(())
    } else {
        Err("系统安装器没收下,详情见 logcat".to_owned())
    }
}

/// 系统安装器回报失败(`Updater.java` 的 `onReceive`)。符号名与那边的包名、类名、
/// 方法名绑死,改一边就要改另一边。
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_osmosis_Updater_nativeInstallFailed<
    'caller,
>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    status: jint,
    message: JString<'caller>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        ui::install_failed(
            status,
            message.try_to_string(env)?,
        );
        Ok(())
    })
    .resolve::<LogErrorAndDefault>();
}

/// 从裸指针重建一份 `JavaVM`。
fn vm(ptr: usize) -> JavaVM {
    // SAFETY:这个指针来自 android-activity 在 `android_main` 之前就拿到的
    // 那个 JavaVM,进程存续期间一直有效(与 downloads.rs 同一条依据)。
    unsafe { JavaVM::from_raw(ptr as *mut _) }
}
