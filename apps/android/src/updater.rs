//! 应用内升级的 Rust 侧:把核对过的 APK 交给 Java 那半边(`gradle/.../Updater.java`)。
//!
//! 下载与校验在 `api::update`,状态与文案在 `ui::update`,这里只是一座桥,
//! 与 `downloads.rs` 同一个形状。

use std::path::Path;
use std::sync::OnceLock;

use jni::JavaVM;

const CLASS: &jni::strings::JNIStr =
    jni::jni_str!("io/github/osmosis/Updater");

/// JavaVM 的裸指针。安装跑在 tokio 的阻塞池上,理由同 `downloads.rs` 的 `VM`。
static VM: OnceLock<usize> = OnceLock::new();

/// 接上安装器。APK 落在应用私有目录的 `update/` 下,不需要任何存储权限。
pub fn start(app: &slint::android::AndroidApp) {
    let Some(dir) = app.internal_data_path() else {
        log::warn!("拿不到应用私有目录,应用内升级不可用");
        return;
    };
    if VM.set(app.vm_as_ptr() as usize).is_err() {
        log::warn!("安装器被接了第二次,后一次没有生效");
        return;
    }
    ui::install_updater(dir.join("update"), install);
}

fn install(apk: &Path) -> Result<(), String> {
    let path =
        apk.to_str().ok_or("安装包路径不是 UTF-8")?;
    let ptr = *VM.get().ok_or("安装器还没接上")?;
    // SAFETY:这个指针来自 android-activity 在 `android_main` 之前就拿到的
    // 那个 JavaVM,进程存续期间一直有效(与 downloads.rs 同一条依据)。
    let vm = unsafe { JavaVM::from_raw(ptr as *mut _) };
    let handed = vm
        .attach_current_thread(|env| {
            let path = env.new_string(path)?;
            env.call_static_method(
                CLASS,
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
