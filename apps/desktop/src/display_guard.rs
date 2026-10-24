//! debug 构建的显示守卫(MEO-1900):AI 会话里起的实例不许开到用户真实的桌面上。
//!
//! AI 执行者该用 `test/headless/` 的无头显示(`just desktop-dev` 默认就走它)。
//! 无头会话给的 `WAYLAND_DISPLAY` 是会话目录里的绝对路径;用户桌面的是
//! `$XDG_RUNTIME_DIR` 下的名字(如 `wayland-1`)或 X11 的 `DISPLAY`。
//! 用户本人要看窗口时带 `OSMOSIS_SHOW_WINDOW=1`。

use std::path::Path;

/// 识别 AI 会话的变量:Claude Code 设 `CLAUDECODE`,Codex 设 `CODEX_THREAD_ID` /
/// `CODEX_SESSION_ID`(2026-10-24 用 codex-cli 0.164.0 实测)。
const AI_SESSION_VARS: [&str; 3] = ["CLAUDECODE", "CODEX_THREAD_ID", "CODEX_SESSION_ID"];

pub const REFUSAL: &str = "AI 会话里不许把 debug 实例开到用户的桌面上(MEO-1900)。\n\
改用无头显示:just desktop-dev(默认无头,MCP 照旧在 8091),或\n\
nix-shell slint.nix --run 'bun test/headless/headless.ts run --dir <目录> --mcp-port 8091'。\n\
用户本人要看窗口时带 OSMOSIS_SHOW_WINDOW=1。";

/// `Err` 表示拒绝启动。`env` 抽出来是为了测试不碰进程环境。
pub fn check(env: impl Fn(&str) -> Option<String>) -> Result<(), &'static str> {
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    let shown = env("OSMOSIS_SHOW_WINDOW").as_deref() == Some("1");
    if shown || !AI_SESSION_VARS.iter().any(|k| set(k)) {
        return Ok(());
    }
    if set("DISPLAY") {
        return Err(REFUSAL);
    }
    let Some(wayland) = env("WAYLAND_DISPLAY").filter(|v| !v.is_empty()) else {
        // 没有任何显示:窗口本来就开不出来,交给后端自己报错。
        return Ok(());
    };
    let path = Path::new(&wayland);
    let runtime = env("XDG_RUNTIME_DIR").unwrap_or_default();
    let on_user_desktop =
        !path.is_absolute() || path.parent().is_some_and(|p| p == Path::new(&runtime));
    if on_user_desktop { Err(REFUSAL) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn run(pairs: &[(&str, &str)]) -> Result<(), &'static str> {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        check(|k| map.get(k).cloned())
    }

    const RT: (&str, &str) = ("XDG_RUNTIME_DIR", "/run/user/1000");

    #[test]
    fn human_session_is_never_refused() {
        assert!(run(&[RT, ("WAYLAND_DISPLAY", "wayland-1")]).is_ok());
    }

    #[test]
    fn claude_on_user_wayland_is_refused() {
        assert!(run(&[RT, ("CLAUDECODE", "1"), ("WAYLAND_DISPLAY", "wayland-1")]).is_err());
    }

    #[test]
    fn codex_on_user_wayland_absolute_is_refused() {
        let r = run(&[RT, ("CODEX_THREAD_ID", "t"), ("WAYLAND_DISPLAY", "/run/user/1000/wayland-1")]);
        assert!(r.is_err());
    }

    #[test]
    fn ai_on_x11_is_refused() {
        assert!(run(&[RT, ("CODEX_SESSION_ID", "s"), ("DISPLAY", ":0")]).is_err());
    }

    #[test]
    fn ai_on_headless_session_is_allowed() {
        let r = run(&[RT, ("CLAUDECODE", "1"), ("WAYLAND_DISPLAY", "/tmp/osmosis-shot-x/run/wayland-1")]);
        assert!(r.is_ok());
    }

    #[test]
    fn explicit_switch_lets_ai_show_the_window() {
        let r = run(&[
            RT,
            ("CLAUDECODE", "1"),
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("OSMOSIS_SHOW_WINDOW", "1"),
        ]);
        assert!(r.is_ok());
    }
}
