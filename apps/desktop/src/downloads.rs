//! 桌面公共音乐目录的待定下载与原子发布。

use std::path::PathBuf;

const PENDING_PREFIX: &str = ".osmosis-download-";
const PENDING_SUFFIX: &str = ".pending";

/// 集中解析音乐目录，供平台注入及后续目录消费者共用。
fn music_directory() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Ok(output) =
        std::process::Command::new("xdg-user-dir")
            .arg("MUSIC")
            .output()
        && output.status.success()
        && let Ok(directory) =
            String::from_utf8(output.stdout)
    {
        let path = PathBuf::from(
            directory.trim_end_matches(['\r', '\n']),
        );
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no home directory for music downloads",
            )
        })?;
    Ok(home.join("Music"))
}

/// 桌面下载统一落在公共音乐目录的 osmosis 子目录。
pub(crate) struct Store {
    /// 本实例的公共落点，不随下载任务变化。
    directory: PathBuf,
}

impl Store {
    /// 平台入口只解析一次落点，目录实际在首次下载时创建。
    pub(crate) fn new() -> std::io::Result<Self> {
        Self::initialize(music_directory()?.join("osmosis"))
    }

    /// 初始化时回收上次进程留下的本应用待定条目。
    fn initialize(
        directory: PathBuf,
    ) -> std::io::Result<Self> {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(err)
                if err.kind()
                    == std::io::ErrorKind::NotFound =>
            {
                return Ok(Self::at(directory));
            }
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(PENDING_PREFIX)
                && name.ends_with(PENDING_SUFFIX)
                && entry.file_type()?.is_file()
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(Self::at(directory))
    }

    /// 测试与平台目录解析共用同一个存储实现。
    fn at(directory: PathBuf) -> Self {
        Self { directory }
    }
}

impl ui::DownloadStore for Store {
    /// 待定字节与正式文件名分离，下载失败不会损坏已有歌曲。
    fn open(
        &self,
        file_name: &str,
    ) -> std::io::Result<(
        Box<dyn std::io::Write + Send>,
        Box<dyn ui::DownloadCommit>,
    )> {
        if file_name.is_empty()
            || file_name == "."
            || file_name == ".."
            || file_name.contains(['/', '\\'])
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid download file name",
            ));
        }
        std::fs::create_dir_all(&self.directory)?;
        let pending = tempfile::Builder::new()
            .prefix(PENDING_PREFIX)
            .suffix(PENDING_SUFFIX)
            .tempfile_in(&self.directory)?;
        let (writer, path) = pending.into_parts();
        Ok((
            Box::new(writer),
            Box::new(Entry {
                path,
                destination: self.directory.join(file_name),
            }),
        ))
    }

    /// 只列正式直接文件，软链和子目录不能扩展音乐目录边界。
    fn list(&self) -> std::io::Result<ui::DownloadListing> {
        let directory =
            match std::fs::read_dir(&self.directory) {
                Ok(directory) => directory,
                Err(err)
                    if err.kind()
                        == std::io::ErrorKind::NotFound =>
                {
                    return Ok(
                        ui::DownloadListing::default(),
                    );
                }
                Err(err) => return Err(err),
            };
        let mut entries = Vec::new();
        for item in directory {
            let item = item?;
            let name = item.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !published_name(name)
                || !item.file_type()?.is_file()
            {
                continue;
            }
            let metadata = item.metadata()?;
            let modified = metadata
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            entries.push(ui::DownloadEntry::from_file(
                name.to_owned(),
                name.to_owned(),
                metadata.len(),
                modified,
            ));
        }
        entries.sort_by(|left, right| {
            right.modified.cmp(&left.modified).then_with(
                || left.file_name.cmp(&right.file_name),
            )
        });
        Ok(ui::DownloadListing {
            entries,
            ..Default::default()
        })
    }

    /// 删除前重新列目录；身份只能来自这份受限快照。
    fn delete(
        &self,
        ids: &[String],
    ) -> std::io::Result<ui::DownloadDeletion> {
        let entries = self.list()?.entries;
        let mut outcome = ui::DownloadDeletion::default();
        let mut seen = std::collections::HashSet::new();
        for id in ids {
            if !seen.insert(id) {
                continue;
            }
            let Some(entry) = entries
                .iter()
                .find(|entry| &entry.id == id)
            else {
                outcome.failures.push(format!(
                    "{id}: 文件不在已下载目录中"
                ));
                continue;
            };
            let path =
                self.directory.join(&entry.file_name);
            // symlink_metadata 不跟随替换成软链的条目。
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    match std::fs::remove_file(&path) {
                        Ok(()) => outcome.deleted.push(
                            ui::DownloadEntry {
                                size: metadata.len(),
                                ..entry.clone()
                            },
                        ),
                        Err(err) => {
                            outcome.failures.push(format!(
                                "{}: {err}",
                                entry.file_name
                            ))
                        }
                    }
                }
                Ok(_) => outcome.failures.push(format!(
                    "{}: 文件类型已改变",
                    entry.file_name
                )),
                Err(err) => outcome.failures.push(format!(
                    "{}: {err}",
                    entry.file_name
                )),
            }
        }
        Ok(outcome)
    }

    /// 下载完成提示使用实际公共目录。
    fn location(&self) -> String {
        self.directory.display().to_string()
    }
}

/// 成品名不含目录分隔符，也不把隐藏待定文件当下载歌曲。
fn published_name(name: &str) -> bool {
    !name.starts_with('.')
        && !name.contains(['/', '\\'])
        && name.ends_with(".mp3")
}

/// 收尾令牌独占临时路径，未提交时由 TempPath 的 Drop 回收。
struct Entry {
    /// 与写句柄分开的待定路径所有权。
    path: tempfile::TempPath,
    /// 首选成品名；同名冲突时追加递增后缀。
    destination: PathBuf,
}

impl ui::DownloadCommit for Entry {
    /// 无覆盖发布是一次原子操作，并发同名下载也保留全部成品。
    fn commit(self: Box<Self>) -> std::io::Result<()> {
        let Self {
            mut path,
            destination,
        } = *self;
        let mut candidate = destination.clone();
        for suffix in 1..=u32::MAX {
            match path.persist_noclobber(&candidate) {
                Ok(()) => return Ok(()),
                Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    path = err.path;
                    let stem = destination.file_stem().expect("open validated a file name").to_string_lossy();
                    let mut name = format!("{stem} ({suffix})");
                    if let Some(extension) = destination.extension() {
                        name.push('.');
                        name.push_str(&extension.to_string_lossy());
                    }
                    candidate = destination.with_file_name(name);
                }
                Err(err) => return Err(err.error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "download file names exhausted",
        ))
    }

    /// 显式断流失败和丢弃令牌共用 TempPath 的清理语义。
    fn discard(self: Box<Self>) {
        if let Err(err) = self.path.close() {
            log::warn!("半截下载文件未能清理: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use similar_asserts::assert_eq;
    use ui::DownloadStore as _;

    /// 只列出已发布的直接文件，外部、待定、嵌套与软链都不可见。
    #[test]
    fn list_contains_only_published_direct_files() {
        let root = tempfile::tempdir().expect("owned root");
        let directory = root.path().join("osmosis");
        std::fs::create_dir(&directory)
            .expect("music directory");
        std::fs::write(
            root.path().join("outside.mp3"),
            b"outside",
        )
        .expect("outside sentinel");
        std::fs::write(
            directory.join("Artist - Title.mp3"),
            b"track",
        )
        .expect("published bytes");
        std::fs::write(
            directory.join(".osmosis-download-X.pending"),
            b"partial",
        )
        .expect("pending");
        std::fs::create_dir(directory.join("nested"))
            .expect("nested directory");
        std::fs::write(
            directory.join("nested/other.mp3"),
            b"nested",
        )
        .expect("nested bytes");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.path().join("outside.mp3"),
            directory.join("linked.mp3"),
        )
        .expect("outside link");
        let entries = Store::at(directory)
            .list()
            .expect("listing")
            .entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].file_name,
            "Artist - Title.mp3"
        );
        assert_eq!(entries[0].size, 5);
        assert_eq!(entries[0].title, "Title");
        assert_eq!(entries[0].artist, "Artist");
        assert!(entries[0].modified > 0);
    }

    /// 不存在的下载目录自然显示空列表，查询不能创建文件。
    #[test]
    fn missing_download_directory_is_empty() {
        let root = tempfile::tempdir().expect("owned root");
        let directory = root.path().join("osmosis");
        assert!(
            Store::at(directory.clone())
                .list()
                .expect("missing directory")
                .entries
                .is_empty()
        );
        assert!(!directory.exists());
    }

    /// 真实删除仅作用于选择的身份，剩余条目的字节保持原样。
    #[test]
    fn delete_removes_only_selected_entries() {
        let root =
            tempfile::tempdir().expect("owned music");
        for (name, bytes) in [
            ("a.mp3", b"a".as_slice()),
            ("b.mp3", b"bb".as_slice()),
            ("c.mp3", b"ccc".as_slice()),
        ] {
            std::fs::write(root.path().join(name), bytes)
                .expect("published file");
        }
        let store = Store::at(root.path().to_path_buf());
        let selected: Vec<_> = store
            .list()
            .expect("listing")
            .entries
            .iter()
            .filter(|item| item.file_name != "b.mp3")
            .map(|item| item.id.clone())
            .collect();
        let result = store
            .delete(&selected)
            .expect("selected deletion");
        assert_eq!(result.deleted.len(), 2);
        assert_eq!(
            result
                .deleted
                .iter()
                .map(|item| item.size)
                .sum::<u64>(),
            4
        );
        assert!(!root.path().join("a.mp3").exists());
        assert!(!root.path().join("c.mp3").exists());
        assert_eq!(
            std::fs::read(root.path().join("b.mp3"))
                .expect("unselected bytes"),
            b"bb"
        );
    }

    /// 伪造身份无法绕过目录边界，也不能跟随外部软链。
    #[test]
    fn forged_ids_cannot_escape_download_directory() {
        let root = tempfile::tempdir().expect("owned root");
        let directory = root.path().join("osmosis");
        std::fs::create_dir(&directory)
            .expect("owned music");
        let outside = root.path().join("outside.mp3");
        std::fs::write(&outside, b"sentinel")
            .expect("outside bytes");
        std::fs::write(
            directory.join("inside.mp3"),
            b"inside",
        )
        .expect("inside bytes");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            &outside,
            directory.join("link.mp3"),
        )
        .expect("link");
        let store = Store::at(directory.clone());
        let ids = vec![
            "../outside.mp3".into(),
            outside.display().to_string(),
            "nested/file.mp3".into(),
            "link.mp3".into(),
            "unknown.mp3".into(),
        ];
        let result = store
            .delete(&ids)
            .expect("invalid identities reported");
        assert!(result.deleted.is_empty());
        assert_eq!(result.failures.len(), ids.len());
        assert_eq!(
            std::fs::read(outside).expect("sentinel kept"),
            b"sentinel"
        );
        assert_eq!(
            std::fs::read(directory.join("inside.mp3"))
                .expect("unselected kept"),
            b"inside"
        );
    }

    /// 子进程内读取环境，避免并发单测修改进程全局 HOME。
    #[test]
    fn music_directory_probe() {
        let Some(expected) =
            std::env::var_os("EXPECTED_MUSIC_DIRECTORY")
        else {
            return;
        };
        assert_eq!(
            music_directory()
                .expect("resolve music directory"),
            PathBuf::from(expected)
        );
    }

    /// 真实工具进程返回 XDG 落点；工具缺失时回退到 HOME/Music。
    #[cfg(target_os = "linux")]
    #[test]
    fn music_directory_uses_xdg_then_home_fallback() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory =
            tempfile::tempdir().expect("owned home");
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin)
            .expect("owned tool directory");
        let shell = std::process::Command::new("sh")
            .args(["-c", "command -v sh"])
            .output()
            .expect("locate shell");
        assert!(shell.status.success());
        let executable = bin.join("xdg-user-dir");
        std::fs::write(&executable, format!("#!{}\nprintf '%s\\n' \"$XDG_TEST_MUSIC\"\n", String::from_utf8(shell.stdout).expect("shell path").trim())).expect("test xdg utility");
        std::fs::set_permissions(
            &executable,
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("executable utility");
        for xdg in [true, false] {
            let expected = if xdg {
                directory.path().join("custom music")
            } else {
                directory.path().join("Music")
            };
            if !xdg {
                std::fs::remove_file(&executable)
                    .expect("remove owned xdg utility");
            }
            let status = std::process::Command::new(
                std::env::current_exe()
                    .expect("test binary"),
            )
            .args([
                "--exact",
                "downloads::tests::music_directory_probe",
                "--nocapture",
            ])
            .env("HOME", directory.path())
            .env("PATH", &bin)
            .env("XDG_TEST_MUSIC", &expected)
            .env("EXPECTED_MUSIC_DIRECTORY", &expected)
            .status()
            .expect("isolated directory probe");
            assert!(
                status.success(),
                "directory resolver failed: xdg={xdg}"
            );
        }
    }

    /// 真实字节只在提交后出现在正式文件名下。
    #[test]
    fn commit_publishes_complete_bytes() {
        let directory = tempfile::tempdir()
            .expect("temporary music directory");
        let store =
            Store::at(directory.path().join("osmosis"));
        let (mut writer, pending) = store
            .open("artist - title.mp3")
            .expect("open download");
        writer
            .write_all(b"complete track")
            .expect("write track");
        assert!(
            !directory
                .path()
                .join("osmosis/artist - title.mp3")
                .exists()
        );
        drop(writer);
        pending.commit().expect("publish track");
        assert_eq!(
            std::fs::read(
                directory
                    .path()
                    .join("osmosis/artist - title.mp3")
            )
            .expect("read track"),
            b"complete track"
        );
        assert_eq!(
            std::fs::read_dir(
                directory.path().join("osmosis")
            )
            .expect("music entries")
            .count(),
            1
        );
    }

    /// 显式失败与未收尾丢弃都清除半截字节。
    #[test]
    fn failed_or_abandoned_download_leaves_no_file() {
        let directory = tempfile::tempdir()
            .expect("temporary music directory");
        let store =
            Store::at(directory.path().to_path_buf());
        for discard in [true, false] {
            let (mut writer, pending) = store
                .open("partial.mp3")
                .expect("open download");
            writer
                .write_all(b"partial")
                .expect("write partial bytes");
            drop(writer);
            if discard {
                pending.discard();
            } else {
                drop(pending);
            }
            assert_eq!(
                std::fs::read_dir(directory.path())
                    .expect("music entries")
                    .count(),
                0
            );
        }
    }

    /// 两个同名下载均保留，已有歌曲也不能被覆盖。
    #[test]
    fn simultaneous_duplicate_names_preserve_every_download()
     {
        let directory = tempfile::tempdir()
            .expect("temporary music directory");
        std::fs::write(
            directory.path().join("song.mp3"),
            b"original",
        )
        .expect("existing track");
        let store =
            Store::at(directory.path().to_path_buf());
        let (mut first, first_pending) =
            store.open("song.mp3").expect("first download");
        let (mut second, second_pending) = store
            .open("song.mp3")
            .expect("second download");
        first.write_all(b"first").expect("first bytes");
        second.write_all(b"second").expect("second bytes");
        drop((first, second));
        first_pending.commit().expect("first publish");
        second_pending.commit().expect("second publish");
        let mut contents: Vec<_> =
            std::fs::read_dir(directory.path())
                .expect("music entries")
                .map(|entry| {
                    std::fs::read(
                        entry.expect("entry").path(),
                    )
                    .expect("track bytes")
                })
                .collect();
        contents.sort();
        assert_eq!(
            contents,
            vec![
                b"first".to_vec(),
                b"original".to_vec(),
                b"second".to_vec()
            ]
        );
    }

    /// 平台接口也拒绝逃出公共音乐目录的文件名。
    #[test]
    fn invalid_names_never_escape_music_directory() {
        let directory = tempfile::tempdir()
            .expect("temporary music directory");
        let store =
            Store::at(directory.path().join("osmosis"));
        for name in [
            "",
            ".",
            "..",
            "../outside.mp3",
            "nested/song.mp3",
            "nested\\song.mp3",
        ] {
            assert!(
                store.open(name).is_err(),
                "accepted invalid name: {name}"
            );
        }
        assert!(
            !directory.path().join("outside.mp3").exists()
        );
    }

    /// 提交失败仍回收临时文件。
    #[test]
    fn failed_publish_cleans_pending_bytes() {
        let directory = tempfile::tempdir()
            .expect("temporary music directory");
        let store =
            Store::at(directory.path().to_path_buf());
        let name = "x".repeat(256);
        let (writer, pending) =
            store.open(&name).expect("open download");
        drop(writer);
        // Linux 文件系统拒绝超过 NAME_MAX 的正式名，临时名仍在可清理的原目录。
        assert!(pending.commit().is_err());
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("music entries")
                .count(),
            0
        );
    }

    /// 初始化只清理本应用的待定文件，其他文件与目录保持原样。
    #[test]
    fn initialization_cleans_only_owned_pending_files() {
        let directory = tempfile::tempdir()
            .expect("owned download directory");
        let pending = directory
            .path()
            .join(".osmosis-download-ABC123.pending");
        std::fs::write(&pending, b"partial download")
            .expect("write stale pending file");
        let preserved = [
            "song.mp3",
            ".osmosis-download-ABC123",
            ".download-ABC123.pending",
            "other.pending",
            ".other-ABC123.pending",
        ];
        for name in preserved {
            std::fs::write(
                directory.path().join(name),
                b"preserved",
            )
            .expect("write unrelated file");
        }
        let unrelated_directory = directory
            .path()
            .join(".osmosis-download-DEF456.pending");
        std::fs::create_dir(&unrelated_directory)
            .expect("create unrelated directory");
        #[cfg(unix)]
        let unrelated_link = {
            let link = directory
                .path()
                .join(".osmosis-download-GHI789.pending");
            std::os::unix::fs::symlink(
                directory.path().join("song.mp3"),
                &link,
            )
            .expect("create unrelated link");
            link
        };
        Store::initialize(directory.path().to_path_buf())
            .expect("initialize download store");
        assert!(
            !pending.exists(),
            "stale pending bytes survived initialization"
        );
        for name in preserved {
            assert_eq!(
                std::fs::read(directory.path().join(name))
                    .expect("read preserved file"),
                b"preserved"
            );
        }
        assert!(unrelated_directory.is_dir());
        #[cfg(unix)]
        assert!(unrelated_link.is_symlink());
    }

    /// 完成消息报告真正的落点。
    #[test]
    fn location_reports_music_directory() {
        let directory = tempfile::tempdir()
            .expect("temporary music directory");
        let store =
            Store::at(directory.path().join("osmosis"));
        assert_eq!(
            store.location(),
            directory
                .path()
                .join("osmosis")
                .display()
                .to_string()
        );
    }
}
