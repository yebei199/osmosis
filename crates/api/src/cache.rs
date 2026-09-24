//! 列表类响应的本地缓存(#123):打开歌单先画上次那份,新的回来再换。
//!
//! 存的是**上一次成功响应的 JSON**,键是请求地址。它只是缓存,真相在服务端:
//! 每次刷新整份覆盖,库坏了或版本不认识就整库丢掉重建,换账号时清空。
//! 读写都在后台线程上做(`platform::off_thread`),不占 UI 线程。

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use rusqlite::{Connection, OptionalExtension as _};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{ApiError, platform};

/// 当前表结构的版本,等于 [`MIGRATIONS`] 的条数。
const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// 按版本顺序排的迁移,版本号记在 sqlite 自带的 `user_version` 里。
/// 第 n 条把库从版本 n 升到 n+1;只许往后追加,不许改已有的。
const MIGRATIONS: &[&str] = &["CREATE TABLE responses (
        key  TEXT PRIMARY KEY,
        body TEXT NOT NULL
    )"];

/// 一个打开的缓存库。
pub(crate) struct Cache {
    conn: Connection,
}

impl Cache {
    /// 打开(必要时新建、迁移、重建)`path` 处的缓存库。连重建都失败时返回 `None`。
    pub(crate) fn open(path: &Path) -> Option<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match Self::migrated(path) {
            Ok(cache) => Some(cache),
            Err(err) => {
                // 它只是缓存:修不如扔,下一次刷新就回来了
                log::warn!("缓存库不可用,丢掉重建: {err}");
                discard(path);
                Self::migrated(path)
                    .inspect_err(|err| {
                        log::warn!("重建缓存库失败: {err}");
                    })
                    .ok()
            }
        }
    }

    /// 打开并迁移到当前版本。坏文件在读版本号这一步就会报错。
    // ponytail: 只查文件头与版本号,不跑 integrity_check;页面级损坏会在 get/put 上报错、读成没有
    fn migrated(path: &Path) -> Result<Self, String> {
        let mut conn = Connection::open(path)
            .map_err(|e| e.to_string())?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        if !(0..=SCHEMA_VERSION).contains(&version) {
            return Err(format!("不认识的版本 {version}"));
        }

        for (done, sql) in MIGRATIONS
            .iter()
            .enumerate()
            .skip(version as usize)
        {
            let tx = conn
                .transaction()
                .map_err(|e| e.to_string())?;
            tx.execute_batch(sql)
                .map_err(|e| e.to_string())?;
            tx.pragma_update(
                None,
                "user_version",
                done + 1,
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        Ok(Self { conn })
    }

    /// 键 `key` 上次存的响应体。读不出来(包括库坏了)当没有。
    pub(crate) fn get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT body FROM responses WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
            .inspect_err(|err| {
                log::warn!("读缓存失败: {err}")
            })
            .ok()
            .flatten()
    }

    /// 存一份响应体,覆盖同键的旧值。
    pub(crate) fn put(&self, key: &str, body: &str) {
        if let Err(err) = self.conn.execute(
            "INSERT OR REPLACE INTO responses (key, body) VALUES (?1, ?2)",
            [key, body],
        ) {
            log::warn!("写缓存失败: {err}");
        }
    }

    /// 清空全部。
    pub(crate) fn clear(&self) {
        if let Err(err) =
            self.conn.execute("DELETE FROM responses", [])
        {
            log::warn!("清缓存失败: {err}");
        }
    }
}

/// 删掉库文件与它的回滚日志。
fn discard(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    let _ = std::fs::remove_file(journal);
}

/// 进程里唯一的缓存库,第一次用到时打开。没有状态目录或打不开就是 `None`,
/// 那时一切照旧走网络,只是没有「先画上次那份」。
///
/// 惰性打开而不是启动时打开:状态目录要等平台入口 `set_state_dir` 之后才定。
fn shared() -> Option<&'static Mutex<Cache>> {
    static SHARED: OnceLock<Option<Mutex<Cache>>> =
        OnceLock::new();
    SHARED
        .get_or_init(|| {
            platform::cache_file()
                .and_then(|path| Cache::open(&path))
                .map(Mutex::new)
        })
        .as_ref()
}

/// `url` 上一次成功的响应。没存过、解不出来(比如表结构没变而 DTO 变了)都当没有。
pub(crate) async fn recall<
    T: DeserializeOwned + Send + 'static,
>(
    url: String,
) -> Option<T> {
    platform::off_thread(move || {
        let body = shared()?.lock().ok()?.get(&url)?;
        serde_json::from_str(&body).ok()
    })
    .await
    .flatten()
}

/// `GET url`,成功就把响应记下来,供下一次 [`recall`]。
pub(crate) async fn fetch<
    T: Serialize + DeserializeOwned + Send + 'static,
>(
    url: String,
) -> Result<T, ApiError> {
    let fresh: T = platform::get_json(url.clone()).await?;
    // 序列化与写盘一起挪到后台,再把值原样带回来 —— 近千首的歌单不在 UI 线程上编码
    let stored = platform::off_thread(move || {
        if let Ok(body) = serde_json::to_string(&fresh)
            && let Some(cache) = shared()
            && let Ok(cache) = cache.lock()
        {
            cache.put(&url, &body);
        }
        fresh
    })
    .await;
    stored.ok_or_else(|| {
        ApiError::Transport(
            "写缓存的后台任务崩了".to_owned(),
        )
    })
}

/// 清空整库。换账号(登录、登出)时调,上一个人的列表不该在下一个人面前闪一下。
pub(crate) fn forget_all() {
    if let Some(cache) = shared()
        && let Ok(cache) = cache.lock()
    {
        cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use similar_asserts::assert_eq;

    use super::{Cache, SCHEMA_VERSION};

    /// 每个用例一个独有的空目录,返回它和里面的库文件路径。
    /// 目录随返回的 `TempDir` 一起删,所以调用方要把它留到用例结束。
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir =
            tempfile::tempdir().expect("建不出临时目录");
        let path = dir.path().join("cache.sqlite3");
        (dir, path)
    }

    fn version_of(path: &Path) -> i64 {
        rusqlite::Connection::open(path)
            .expect("打不开库文件")
            .query_row("PRAGMA user_version", [], |row| {
                row.get(0)
            })
            .expect("读不出版本号")
    }

    /// 写进去什么,读出来就是什么。
    #[test]
    fn stored_body_reads_back_unchanged() {
        let (_dir, path) = scratch();
        let cache = Cache::open(&path).expect("开不了缓存");

        cache.put(
            "/liked",
            r#"{"tracks":[],"unavailable":0}"#,
        );

        assert_eq!(
            cache.get("/liked").as_deref(),
            Some(r#"{"tracks":[],"unavailable":0}"#)
        );
    }

    /// 没存过的键读出来是没有,而不是空串或报错。
    #[test]
    fn missing_key_reads_as_none() {
        let (_dir, path) = scratch();
        let cache = Cache::open(&path).expect("开不了缓存");

        assert_eq!(cache.get("/daily"), None);
    }

    /// 服务端那份变了(比如在别的设备上取消了喜欢),刷新写回来的新响应盖掉旧的。
    #[test]
    fn later_response_overwrites_earlier() {
        let (_dir, path) = scratch();
        let cache = Cache::open(&path).expect("开不了缓存");

        cache.put(
            "/liked/ids",
            r#"{"track_ids":["a","b"]}"#,
        );
        cache.put("/liked/ids", r#"{"track_ids":["a"]}"#);

        assert_eq!(
            cache.get("/liked/ids").as_deref(),
            Some(r#"{"track_ids":["a"]}"#)
        );
    }

    /// 从一个还不存在的文件开始,迁移一路跑到当前版本。
    #[test]
    fn fresh_file_is_migrated_to_current_version() {
        let (_dir, path) = scratch();

        let cache = Cache::open(&path).expect("开不了缓存");
        drop(cache);

        assert_eq!(version_of(&path), SCHEMA_VERSION);
    }

    /// 重开一个已经是当前版本的库,里面的东西还在。
    #[test]
    fn reopening_keeps_stored_bodies() {
        let (_dir, path) = scratch();
        Cache::open(&path)
            .expect("开不了缓存")
            .put("/playlists", "old");

        let cache =
            Cache::open(&path).expect("重开不了缓存");

        assert_eq!(
            cache.get("/playlists").as_deref(),
            Some("old")
        );
    }

    /// 库文件坏了(这里是一堆不是 sqlite 的字节):丢掉重建,不崩,之后照常能用。
    #[test]
    fn corrupt_file_is_discarded_and_rebuilt() {
        let (_dir, path) = scratch();
        std::fs::write(
            &path,
            b"not a sqlite database ".repeat(512),
        )
        .expect("写不出坏文件");

        let cache =
            Cache::open(&path).expect("坏库没被重建");
        cache.put("/daily", "fresh");

        assert_eq!(
            cache.get("/daily").as_deref(),
            Some("fresh")
        );
        assert_eq!(version_of(&path), SCHEMA_VERSION);
    }

    /// 版本号比认识的还新(装回了旧版本的包):不猜它的表结构,整库丢掉重建。
    #[test]
    fn unknown_newer_version_is_discarded() {
        let (_dir, path) = scratch();
        Cache::open(&path)
            .expect("开不了缓存")
            .put("/daily", "from the future");
        rusqlite::Connection::open(&path)
            .expect("打不开库文件")
            .pragma_update(
                None,
                "user_version",
                SCHEMA_VERSION + 1,
            )
            .expect("改不了版本号");

        let cache = Cache::open(&path).expect("开不了缓存");

        assert_eq!(cache.get("/daily"), None);
        assert_eq!(version_of(&path), SCHEMA_VERSION);
    }

    /// 换账号时清掉:上一个人的红心不该在下一个人的列表里闪一下。
    #[test]
    fn clear_forgets_everything() {
        let (_dir, path) = scratch();
        let cache = Cache::open(&path).expect("开不了缓存");
        cache.put("/daily", "a");
        cache.put("/liked", "b");

        cache.clear();

        assert_eq!(cache.get("/daily"), None);
        assert_eq!(cache.get("/liked"), None);
    }
}
