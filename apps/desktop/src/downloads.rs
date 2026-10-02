//! 桌面公共音乐目录的待定下载与原子发布。

use std::path::PathBuf;

/// 集中解析音乐目录，供平台注入及后续目录消费者共用。
fn music_directory() -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "music directory resolution is not implemented",
    ))
}

/// 桌面下载统一落在公共音乐目录的 osmosis 子目录。
pub(crate) struct Store {
    /// 本实例的公共落点，不随下载任务变化。
    directory: PathBuf,
}

impl Store {
    /// 测试与平台目录解析共用同一个存储实现。
    fn at(directory: PathBuf) -> Self {
        Self { directory }
    }
}

impl ui::DownloadStore for Store {
    fn open(
        &self,
        _file_name: &str,
    ) -> std::io::Result<(
        Box<dyn std::io::Write + Send>,
        Box<dyn ui::DownloadCommit>,
    )> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "desktop download store is not implemented",
        ))
    }

    fn location(&self) -> String {
        self.directory.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::DownloadStore as _;

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
    #[cfg(unix)]
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
