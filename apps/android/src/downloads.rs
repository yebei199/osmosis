//! 下载落盘的 Rust 侧:把字节交给系统的公共「音乐」目录。
//!
//! 真正干活的是 Java 那半边(`gradle/.../Downloads.java`)—— MediaStore 是
//! ContentResolver 上的一组调用,没有 NDK 接口,与媒体控件同一个处境
//! (`docs/adr/0020`)。这个模块只是那两侧之间的桥,不记任何状态。
//!
//! 条目**先建成待定的**,写完才提交:断网时提交换成删除,音乐库里因此不会
//! 留下一首放到一半就停的歌(那种文件看起来跟正常的一模一样)。

use std::os::fd::FromRawFd as _;
use std::sync::OnceLock;

use jni::JavaVM;
use jni::objects::JValue;

/// Java 侧那个类的全名。三处调用共用,写错的现象是 `NoClassDefFoundError`,
/// 而且要等用户第一次点下载才抛。
const CLASS: &jni::strings::JNIStr =
    jni::jni_str!("io/github/osmosis/Downloads");

/// JavaVM 的裸指针。
///
/// JNI 调用会发生在任意线程上(下载跑在 tokio 的工作线程上),而 `AndroidApp`
/// 只在 `android_main` 的参数上出现一次 —— 与 `controls.rs` 的命令通道同一个理由,
/// 这条通道只能是全局的。存指针而不是 `JavaVM`:它不是 `Sync`,而从它重建一份
/// 是零成本的。
static VM: OnceLock<usize> = OnceLock::new();

/// 接上落点。接不上就退回什么都不做 —— 没有下载不影响听歌。
pub fn start(
    app: &slint::android::AndroidApp,
) -> Box<dyn ui::DownloadStore> {
    if VM.set(app.vm_as_ptr() as usize).is_err() {
        log::warn!("下载落点被接了第二次,后一次没有生效");
    }
    Box::new(Store)
}

struct Store;

/// 待收尾的那个条目。只有一个令牌 —— Uri 与文件描述符都在 Java 那边。
struct Entry {
    token: i64,
}

impl ui::DownloadStore for Store {
    fn open(
        &self,
        file_name: &str,
    ) -> std::io::Result<(
        Box<dyn std::io::Write + Send>,
        Box<dyn ui::DownloadCommit>,
    )> {
        let (token, fd) =
            open_entry(file_name).map_err(as_io)?;
        if token == 0 {
            return Err(std::io::Error::other(
                "系统没给出可写的条目",
            ));
        }
        if fd < 0 {
            // 条目建出来了却开不了句柄:不删掉的话它会以待定态永远挂在那里。
            let _ = finish(token, false);
            return Err(std::io::Error::other(
                "开不了写句柄",
            ));
        }

        // SAFETY:`detachFd` 把这个描述符的所有权交了出来 —— Java 那边不再
        // 持有它,也不会关它。`File` 从此是它唯一的主人。
        let file =
            unsafe { std::fs::File::from_raw_fd(fd) };
        Ok((Box::new(file), Box::new(Entry { token })))
    }

    fn location(&self) -> String {
        // 与 Downloads.java 的 RELATIVE_PATH 对应,写成用户在文件管理器里
        // 看到的样子(系统把 Music 显示成「音乐」)。
        "音乐/osmosis".to_owned()
    }
}

impl ui::DownloadCommit for Entry {
    fn commit(self: Box<Self>) -> std::io::Result<()> {
        match finish(self.token, true) {
            Ok(true) => Ok(()),
            Ok(false) => Err(std::io::Error::other(
                "系统没认下这个条目",
            )),
            Err(err) => Err(as_io(err)),
        }
    }

    fn discard(self: Box<Self>) {
        // 删不掉只是多一个待定条目(用户看不见它),不值得再往上报一层。
        if let Err(err) = finish(self.token, false) {
            log::warn!("半截文件没能删掉: {err}");
        }
    }
}

/// 建一个待定条目并取走它的写句柄。返回 (令牌, 文件描述符)。
fn open_entry(
    file_name: &str,
) -> jni::errors::Result<(i64, i32)> {
    vm().attach_current_thread(|env| {
        let name = env.new_string(file_name)?;
        let token = env
            .call_static_method(
                CLASS,
                jni::jni_str!("open"),
                jni::jni_sig!("(Ljava/lang/String;)J"),
                &[(&name).into()],
            )?
            .j()?;
        if token == 0 {
            return Ok((0, -1));
        }

        let fd = env
            .call_static_method(
                CLASS,
                jni::jni_str!("detachFd"),
                jni::jni_sig!("(J)I"),
                &[JValue::Long(token)],
            )?
            .i()?;
        Ok((token, fd))
    })
}

/// 收尾:`keep` 为真让文件对系统可见,为假把它连同记录一起删掉。
fn finish(
    token: i64,
    keep: bool,
) -> jni::errors::Result<bool> {
    vm().attach_current_thread(|env| {
        env.call_static_method(
            CLASS,
            jni::jni_str!("finish"),
            jni::jni_sig!("(JZ)Z"),
            &[JValue::Long(token), JValue::Bool(keep)],
        )?
        .z()
    })
}

/// 从存下来的裸指针重建一份 `JavaVM`。
fn vm() -> JavaVM {
    let ptr = *VM
        .get()
        .expect("下载落点还没接上 —— start 必须先跑");
    // SAFETY:这个指针来自 android-activity 在 `android_main` 之前就拿到的
    // 那个 JavaVM,进程存续期间一直有效(与 controls.rs 同一条依据)。
    unsafe { JavaVM::from_raw(ptr as *mut _) }
}

/// JNI 的失败对上层就是"没存下来"。
fn as_io(err: jni::errors::Error) -> std::io::Error {
    std::io::Error::other(err.to_string())
}
