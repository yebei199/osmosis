use super::*;

use crate::platform;

/// 四件事写在**一个**测试里而不是四个:token 是进程级的全局状态,
/// 拆成四个测试会并行地互相踩,而「一开始没有」那条还依赖执行顺序。
#[test]
fn the_session_token_has_a_lifecycle() {
    // 别的模块也有碰 token 的测试,与它们串起来跑
    let _guard = super::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // 用一个临时文件当会话落盘处,免得动到真实的那一份
    let dir = std::env::temp_dir()
        .join("osmosis-session-lifecycle");
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: 单线程测试起点,此时还没有别的线程在读环境
    unsafe {
        std::env::set_var(
            "OSMOSIS_SESSION_FILE",
            dir.join("session"),
        );
    }

    self::clear();
    assert_eq!(self::token(), None, "一开始不该有 token");

    self::set("first");
    assert_eq!(self::token().as_deref(), Some("first"));

    self::set("second");
    assert_eq!(
        self::token().as_deref(),
        Some("second"),
        "换账号登录后带的该是新 token"
    );

    self::clear();
    assert_eq!(self::token(), None, "登出后不该还留着");
}

/// **被服务端判失效时,会话文件先留一份备份再清**(#127)。
///
/// 删会话不可逆:判错一次(比如开发实例拿别的后端的 token 问了一圈),
/// 用户就得重登,而且事后查不出删掉的是哪一份。登出不走这里,登出就该删。
#[test]
fn an_expired_session_is_backed_up_before_it_is_cleared() {
    let _guard = super::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let dir =
        std::env::temp_dir().join("osmosis-session-expire");
    let _ = std::fs::remove_dir_all(&dir);
    let file = dir.join("session");
    // SAFETY: 拿着 TEST_LOCK,此刻没有别的测试在读写这个变量
    unsafe {
        std::env::set_var("OSMOSIS_SESSION_FILE", &file);
    }

    self::set("rejected-token");
    assert!(self::expire_if_current(Some(
        "rejected-token"
    )));

    assert_eq!(self::token(), None, "失效之后不该还带着它");
    assert!(
        !file.exists(),
        "会话文件该清掉,否则重启又恢复出这个坏 token"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("session.bak"))
            .ok()
            .as_deref(),
        Some("rejected-token"),
        "清之前该留一份备份"
    );
}

/// **入口显式给的状态目录压过环境变量。**
///
/// 安卓上两个环境变量都没有,私有目录只有平台入口那一层拿得到
/// (`apps/android` 的 `internal_data_path`),给了它就该用它。
#[test]
fn session_path_prefers_the_explicit_state_dir() {
    let path = platform::session_path_from(
        Some(std::path::Path::new(
            "/data/user/0/app/files",
        )),
        Some("/tmp/state"),
        Some("/home/someone"),
    )
    .expect("给了显式目录就该有路径");

    assert!(path.starts_with("/data/user/0/app/files"));
    assert!(path.ends_with(format!(
        "{}/session",
        platform::APP_DIR
    )));
}

/// 边界:安卓上除了显式目录什么都没有,那时也要落得下来。
#[test]
fn an_explicit_state_dir_works_without_any_env() {
    let path = platform::session_path_from(
        Some(std::path::Path::new(
            "/data/user/0/app/files",
        )),
        None,
        None,
    )
    .expect("只有显式目录也该有路径");

    assert!(path.ends_with(format!(
        "{}/session",
        platform::APP_DIR
    )));
}

/// 有 XDG_STATE_HOME 就用它 —— 登录态是状态不是配置。
#[test]
fn session_path_prefers_state_home() {
    let path = platform::session_path_from(
        None,
        Some("/tmp/state"),
        Some("/home/someone"),
    )
    .expect("给了 state home 就该有路径");

    assert!(path.starts_with("/tmp/state"));
    assert!(path.ends_with(format!(
        "{}/session",
        platform::APP_DIR
    )));
}

/// 没有 XDG_STATE_HOME 就退到 HOME/.local/state。
#[test]
fn session_path_falls_back_to_home() {
    let path = platform::session_path_from(
        None,
        None,
        Some("/home/someone"),
    )
    .expect("有 HOME 就该有路径");

    assert!(path.starts_with("/home/someone/.local/state"));
}

/// **连本机后端的构建与连生产的构建各用一个目录**(#127)。
///
/// 两者共用一份会话时,开发实例拿生产 token 去问本机后端必然 401,
/// 于是把共用的会话文件删了,装机版跟着掉登录。token 只在签发它的那个
/// 后端作数,所以按「烘进来的后端地址」分,而不是按构建档分。
#[test]
fn the_dev_backend_gets_its_own_state_dir() {
    assert_eq!(platform::app_dir(None), "osmosis-dev");
    assert_eq!(
        platform::app_dir(Some(
            "https://music.cryptorust.uk"
        )),
        "osmosis",
        "装机版沿用原来的目录,更新之后才读得到已有的登录态"
    );
}

/// 两个都没有时不猜一个路径出来 —— 安卓上就是这种情况,
/// 猜错了写进去,失败还是静默的。空串等同于没有。
#[test]
fn session_path_is_none_without_either() {
    assert_eq!(
        platform::session_path_from(None, None, None),
        None
    );
    assert_eq!(
        platform::session_path_from(
            None,
            Some(""),
            Some("")
        ),
        None
    );
}

/// **同一台设备冷启动后还是同一个 id。**
///
/// 遥控器重连靠 `ClaimControl { resume }` 按 id 认人(#95):id 每次启动都变的话
/// 永远走不到那一支,被控端会一直被一个已经不存在的设备锁着。
#[test]
fn the_device_id_survives_a_restart() {
    // 与别的碰进程环境的测试串起来跑
    let _guard = super::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // 指到临时文件,免得动到真实的那一份;上一轮留下的也要清掉
    let dir =
        std::env::temp_dir().join("osmosis-device-id");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: 拿着 TEST_LOCK,此刻没有别的测试在读写这个变量
    unsafe {
        std::env::set_var(
            "OSMOSIS_DEVICE_FILE",
            dir.join("device"),
        );
    }

    assert_eq!(
        self::device_id(|| "pc1-1234".to_owned()),
        "pc1-1234",
        "第一次启动该用现算的那个"
    );
    assert_eq!(
        // 第二次冷启动:进程号换了
        self::device_id(|| "pc1-5678".to_owned()),
        "pc1-1234",
        "冷启动后该还是同一台设备"
    );
}

/// **重启之后还登着。**
///
/// 桌面端的回归(用户 2026-09-20 要求两端一并验收):`restore` 是那条唯一把
/// 盘上的 token 搬回内存的路,而它一直没有测试盯着 —— 安卓这轮改的是它读的
/// 那个目录,读回来这一步坏了两端一起掉登录。
///
/// 两个方向都断言:存过的要回得来,没存过的不能凭空登上(登出之后文件被删掉,
/// 这时若还能恢复出一个 token,登出就等于没登出)。
#[test]
fn a_saved_session_comes_back_after_a_restart() {
    let _guard = super::TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let dir = std::env::temp_dir()
        .join("osmosis-session-restore");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    // SAFETY: 拿着 TEST_LOCK,此刻没有别的测试在读写这个变量
    unsafe {
        std::env::set_var(
            "OSMOSIS_SESSION_FILE",
            dir.join("session"),
        );
    }

    self::set("kept-token");
    // 模拟一次冷启动:进程内存里那份没了,盘上那份还在
    forget_in_memory();
    assert_eq!(
        self::token(),
        None,
        "重启的起点是内存里没有"
    );

    self::restore();
    assert_eq!(
        self::token().as_deref(),
        Some("kept-token"),
        "重开该还登着 —— 落盘的那份没搬回来"
    );

    // 登出把文件删掉,那之后的"重启"不该恢复出任何东西
    self::clear();
    forget_in_memory();
    self::restore();
    assert_eq!(
        self::token(),
        None,
        "登出之后重开不该还登着"
    );
}

/// 只清掉内存里那份 token,盘上的不动 —— 进程重启看起来就是这样。
#[cfg(test)]
fn forget_in_memory() {
    if let Ok(mut slot) = super::TOKEN.write() {
        *slot = None;
    }
}

/// 存了再读,拿回同一个 token —— 这是"下次启动还登着"的全部含义。
#[test]
fn session_survives_a_restart() {
    let path = std::env::temp_dir()
        .join("osmosis-session-restart/session");
    platform::write_session(&path, "kept");

    let read = std::fs::read_to_string(&path)
        .expect("刚写的文件该读得到");

    assert_eq!(read.trim(), "kept");
    let _ = std::fs::remove_file(&path);
}

/// 会话文件权限是 0600 —— token 等同于密码。
#[cfg(unix)]
#[test]
fn session_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;

    let path = std::env::temp_dir()
        .join("osmosis-session-perm/session");
    platform::write_session(&path, "secret");

    let mode = std::fs::metadata(&path)
        .expect("刚写的文件该在")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(mode, 0o600, "会话文件权限应为 0600");
    let _ = std::fs::remove_file(&path);
}

