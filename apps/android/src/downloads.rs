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
use jni::objects::{JObjectArray, JString, JValue};
use jni::refs::IntoAuto as _;

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

    /// MediaStore严格限定公共音乐子目录，受限权限说明随列表交回。
    fn list(&self) -> std::io::Result<ui::DownloadListing> {
        let fields =
            read_array(jni::jni_str!("list"), None)?;
        if fields.len() < 2 {
            return Err(std::io::Error::other(
                "invalid MediaStore listing",
            ));
        }
        Ok(ui::DownloadListing {
            note: fields[0].clone(),
            pending: fields[1] == "true",
            entries: parse_entries(&fields[2..])?,
        })
    }

    /// Java侧重新查目录，仅接收数字MediaStore身份，不接收Uri。
    fn delete(
        &self,
        ids: &[String],
    ) -> std::io::Result<ui::DownloadDeletion> {
        if ids.iter().any(|id| {
            id.is_empty()
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
        }) {
            return Err(std::io::Error::other(
                "invalid MediaStore download id",
            ));
        }
        parse_deletion(read_array(
            jni::jni_str!("delete"),
            Some(&ids.join("\n")),
        )?)
    }

    /// 读取权限请求在Java Activity主线程执行。
    fn request_access(&self) -> std::io::Result<()> {
        vm().attach_current_thread(|env| {
            let result = env.call_static_method(
                CLASS,
                jni::jni_str!("requestAccess"),
                jni::jni_sig!("()V"),
                &[],
            );
            if result.is_err() {
                env.exception_clear();
            }
            result.map(|_| ())
        })
        .map_err(as_io)
    }

    /// 系统确认尚未结束返回None，取消和失败都有可消费终态。
    fn poll_delete(
        &self,
    ) -> std::io::Result<Option<ui::DownloadDeletion>> {
        let fields =
            read_array(jni::jni_str!("pollDelete"), None)?;
        if fields.is_empty() {
            return Ok(None);
        }
        parse_deletion(fields).map(Some)
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

/// 字符串数组跨JNI保留完整文件名，避免分隔符破坏任意用户文本。
fn read_array(
    method: &'static jni::strings::JNIStr,
    argument: Option<&str>,
) -> std::io::Result<Vec<String>> {
    vm().attach_current_thread(|env| {
        let result = (|| {
            let object = if let Some(argument) = argument {
                let argument = env.new_string(argument)?;
                env.call_static_method(CLASS, method, jni::jni_sig!("(Ljava/lang/String;)[Ljava/lang/String;"), &[(&argument).into()])?.l()?
            } else {
                env.call_static_method(CLASS, method, jni::jni_sig!("()[Ljava/lang/String;"), &[])?.l()?
            };
            let array = JObjectArray::<JString>::cast_local(env, object)?;
            let mut fields = Vec::with_capacity(array.len(env)?);
            for index in 0..array.len(env)? {
                let item = array.get_element(env, index)?;
                let item = item.auto();
                fields.push(item.try_to_string(env)?);
            }
            Ok(fields)
        })();
        if result.is_err() { env.exception_clear(); }
        result
    }).map_err(as_io)
}

/// 四字段分组有明确长度检查，错误数据不能变成错误文件删除。
fn parse_entries(
    fields: &[String],
) -> std::io::Result<Vec<ui::DownloadEntry>> {
    if !fields.len().is_multiple_of(4) {
        return Err(std::io::Error::other(
            "invalid MediaStore entry fields",
        ));
    }
    fields
        .chunks_exact(4)
        .map(|entry| {
            let size = entry[2]
                .parse::<u64>()
                .map_err(std::io::Error::other)?;
            let modified = entry[3]
                .parse::<u64>()
                .map_err(std::io::Error::other)?;
            Ok(ui::DownloadEntry::from_file(
                entry[0].clone(),
                entry[1].clone(),
                size,
                modified,
            ))
        })
        .collect()
}

/// 系统终态与实际删除列表分开解析，不凭成功按钮推测释放量。
fn parse_deletion(
    fields: Vec<String>,
) -> std::io::Result<ui::DownloadDeletion> {
    let start = deletion_entries_start(&fields)?;
    if !["pending", "cancelled", "complete"]
        .contains(&fields[0].as_str())
    {
        return Err(std::io::Error::other(
            "invalid system deletion state",
        ));
    }
    Ok(ui::DownloadDeletion {
        deleted: parse_entries(&fields[start..])?,
        failures: fields[2..start].to_vec(),
        pending: fields[0] == "pending",
        cancelled: fields[0] == "cancelled",
    })
}

/// 验证变长失败列表的头部，定位后续已删除条目。
fn deletion_entries_start(
    fields: &[String],
) -> std::io::Result<usize> {
    if fields.len() < 2 {
        return Err(std::io::Error::other(
            "invalid MediaStore deletion result",
        ));
    }
    let count = fields[1]
        .parse::<usize>()
        .map_err(std::io::Error::other)?;
    count
        .checked_add(2)
        .filter(|start| *start <= fields.len())
        .ok_or_else(|| {
            std::io::Error::other(
                "invalid deletion failure count",
            )
        })
}
