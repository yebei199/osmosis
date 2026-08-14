use super::*;

use crate::platform;

/// 四件事写在**一个**测试里而不是四个:token 是进程级的全局状态,
/// 拆成四个测试会并行地互相踩,而「一开始没有」那条还依赖执行顺序。
#[test]
fn the_session_token_has_a_lifecycle() {
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

/// 有 XDG_STATE_HOME 就用它 —— 登录态是状态不是配置。
#[test]
fn session_path_prefers_state_home() {
    let path = platform::session_path_from(
        Some("/tmp/state"),
        Some("/home/someone"),
    )
    .expect("给了 state home 就该有路径");

    assert!(path.starts_with("/tmp/state"));
    assert!(path.ends_with("osmosis/session"));
}

/// 没有 XDG_STATE_HOME 就退到 HOME/.local/state。
#[test]
fn session_path_falls_back_to_home() {
    let path = platform::session_path_from(
        None,
        Some("/home/someone"),
    )
    .expect("有 HOME 就该有路径");

    assert!(path.starts_with("/home/someone/.local/state"));
}

/// 两个都没有时不猜一个路径出来 —— 安卓上就是这种情况,
/// 猜错了写进去,失败还是静默的。空串等同于没有。
#[test]
fn session_path_is_none_without_either() {
    assert_eq!(
        platform::session_path_from(None, None),
        None
    );
    assert_eq!(
        platform::session_path_from(Some(""), Some("")),
        None
    );
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
