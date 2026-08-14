use super::*;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::sweep_dir;

/// 建一个空的临时目录,名字带上用例名免得两个用例互相踩。
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("osmosis-sweep-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建不出临时目录");
    dir
}

/// 写一个指定大小、指定"有多旧"的文件。
///
/// mtime 用 `File::set_modified` 精确设定,而不是靠 sleep 拉开时间差 ——
/// 那种测试在慢机器上会时好时坏。
fn file(
    dir: &Path,
    name: &str,
    size: usize,
    age_secs: u64,
) {
    let path = dir.join(name);
    std::fs::write(&path, vec![0u8; size])
        .expect("写不出测试文件");
    let handle = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("打不开测试文件");
    handle
        .set_modified(
            SystemTime::now()
                - Duration::from_secs(age_secs),
        )
        .expect("设不了 mtime");
}

fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("读不到临时目录")
        .filter_map(|entry| {
            Some(
                entry
                    .ok()?
                    .file_name()
                    .to_string_lossy()
                    .into_owned(),
            )
        })
        .collect();
    found.sort();
    found
}

/// 超出预算时从最旧的删起,删到线下就停手。
///
/// 删过头的现象是刚看过的那一屏封面下次还要重取 —— 缓存在,却总不命中。
#[test]
fn the_sweep_deletes_oldest_first_until_under_budget() {
    let dir = scratch("oldest-first");
    file(&dir, "old", 100, 300);
    file(&dir, "mid", 100, 200);
    file(&dir, "new", 100, 100);

    // 预算 250:删掉最旧那一个就到 200,不该再动第二个
    sweep_dir(&dir, 250);

    assert_eq!(names(&dir), vec!["mid", "new"]);
}

/// 没超预算时一个都不删 —— 清理不该在正常情况下动手。
#[test]
fn the_sweep_keeps_everything_under_budget() {
    let dir = scratch("under-budget");
    file(&dir, "a", 100, 200);
    file(&dir, "b", 100, 100);

    sweep_dir(&dir, 1024);

    assert_eq!(names(&dir), vec!["a", "b"]);
}

/// 目录还不存在时安静返回 —— 第一次启动就是这个样子,不是故障。
#[test]
fn the_sweep_tolerates_a_missing_directory() {
    let dir = scratch("missing").join("not-created-yet");
    sweep_dir(&dir, 0);
    assert!(!dir.exists());
}
