//! 日志落盘(#145):装机版的 stdout/stderr 接的是 /dev/null,出事时什么都留不下。
//!
//! env_logger 自己不落文件也不滚动;为这点事不值得换一套日志框架，于是在它的
//! `Target::Pipe` 上挂一个 [`Tee`]:照旧写 stderr,再写一份进带大小上限的 [`RollingFile`]。

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// 单个日志文件的上限。
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// 留几个旧文件。
const KEEP: usize = 2;

/// 日志文件名，放在状态目录里(与会话、设置同一处)。
const FILE_NAME: &str = "osmosis.log";

/// 装好全局 logger:级别默认 `info`,认 `RUST_LOG`;stderr 照常，另写一份进状态目录。
/// 文件开不了就只写 stderr,不因为日志起不来而不让应用起。
pub fn init() {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or("info"),
    );
    let file = api::state_file(FILE_NAME).map(|path| {
        let opened = RollingFile::open(
            path.clone(),
            MAX_BYTES,
            KEEP,
        );
        (path, opened)
    });
    let failed = match file {
        Some((_, Ok(file))) => {
            builder.target(env_logger::Target::Pipe(
                Box::new(Tee(file)),
            ));
            None
        }
        Some((path, Err(error))) => {
            Some(format!("{}: {error}", path.display()))
        }
        None => Some("找不到状态目录".into()),
    };
    builder.init();
    // 每次启动的第一行：对得上是哪一版、哪一次运行(时刻在行首，由 env_logger 带)。
    log::info!(
        "osmosis-desktop {} 启动,pid {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    if let Some(why) = failed {
        log::warn!("日志文件开不了，只写 stderr:{why}");
    }
}

/// 写两份:stderr 与日志文件。stderr 那份写不出去(没接终端)不影响文件。
struct Tee(RollingFile);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        self.0.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

/// 带大小上限的日志文件。
///
/// 写满 `max_bytes` 就滚:`path` 改名成 `path.1`,原来的 `path.1` 改成 `path.2`,
/// 依此类推，最多留 `keep` 个旧文件。打开时接着往后写，不清空。
pub struct RollingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: File,
    written: u64,
}

impl RollingFile {
    pub fn open(
        path: PathBuf,
        max_bytes: u64,
        keep: usize,
    ) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = append(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            max_bytes,
            keep,
            file,
            written,
        })
    }

    fn roll(&mut self) -> io::Result<()> {
        // 最老的先腾位置：Windows 上 rename 不覆盖已有文件
        let _ = std::fs::remove_file(numbered(
            &self.path, self.keep,
        ));
        for n in (1..self.keep).rev() {
            let _ = std::fs::rename(
                numbered(&self.path, n),
                numbered(&self.path, n + 1),
            );
        }
        if self.keep > 0 {
            std::fs::rename(
                &self.path,
                numbered(&self.path, 1),
            )?;
        } else {
            std::fs::remove_file(&self.path)?;
        }
        self.file = append(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// `path` 后面接 `.n`。
fn numbered(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

impl Write for RollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // 空文件不滚：单条比上限还大时照样写进去，不换出一串空文件
        if self.written > 0
            && self.written + buf.len() as u64
                > self.max_bytes
        {
            self.roll()?;
        }
        self.file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    fn old(path: &Path, n: usize) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    /// 写满上限就换新文件，旧的往后挪一格;旧文件最多留 `keep` 个，更老的丢掉。
    #[test]
    fn a_full_file_rolls_over_and_old_files_stay_within_keep()
     {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("osmosis.log");
        let mut file =
            RollingFile::open(path.clone(), 10, 2).unwrap();
        for line in [
            "aaaa\n", "bbbb\n", "cccc\n", "dddd\n",
            "eeee\n", "ffff\n", "gggg\n",
        ] {
            file.write_all(line.as_bytes()).unwrap();
        }

        assert_eq!(
            read(&path),
            "gggg\n",
            "当前文件只有最新的"
        );
        assert_eq!(read(&old(&path, 1)), "eeee\nffff\n");
        assert_eq!(read(&old(&path, 2)), "cccc\ndddd\n");
        assert!(
            !old(&path, 3).exists(),
            "超出 keep 的旧文件不该留下"
        );
    }

    /// 单条比上限还大时照样写进去：空文件不再滚，免得一条长日志换出一串空文件。
    #[test]
    fn one_oversized_record_still_lands_in_a_fresh_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("osmosis.log");
        let mut file =
            RollingFile::open(path.clone(), 4, 1).unwrap();
        file.write_all(b"0123456789\n").unwrap();

        assert_eq!(read(&path), "0123456789\n");
        assert!(!old(&path, 1).exists());
    }

    /// 重启不清空：出事之后用户重开应用，上一次运行的日志还在。
    #[test]
    fn reopening_appends_to_what_the_last_run_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("osmosis.log");
        RollingFile::open(path.clone(), 1_000, 1)
            .unwrap()
            .write_all(b"first run\n")
            .unwrap();
        RollingFile::open(path.clone(), 1_000, 1)
            .unwrap()
            .write_all(b"second run\n")
            .unwrap();

        assert_eq!(read(&path), "first run\nsecond run\n");
    }

    /// 接着上一次写时，上一次留下的大小也算进上限。
    #[test]
    fn the_size_left_by_the_last_run_counts_toward_the_cap()
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("osmosis.log");
        RollingFile::open(path.clone(), 10, 1)
            .unwrap()
            .write_all(b"12345678\n")
            .unwrap();
        RollingFile::open(path.clone(), 10, 1)
            .unwrap()
            .write_all(b"abcd\n")
            .unwrap();

        assert_eq!(read(&path), "abcd\n");
        assert_eq!(read(&old(&path, 1)), "12345678\n");
    }
}
