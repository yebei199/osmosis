//! 跑外部命令、找仓库根。xtask 里所有与"外部世界"打交道的部分都在这里。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 仓库根目录。
///
/// `CARGO_MANIFEST_DIR` 在编译期就被 cargo 填成 `<仓库根>/xtask`,因此
/// 无论从哪个目录调用 `cargo xtask`,这里拿到的都是同一个绝对路径。
pub fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ 必须位于仓库根之下")
}

/// 在仓库根下跑一条命令,失败即返回错误。
pub fn run(
    program: &str,
    args: &[&str],
) -> Result<(), String> {
    run_in(repo_root(), program, args)
}

/// 在指定目录下跑一条命令。
pub fn run_in(
    directory: &Path,
    program: &str,
    args: &[&str],
) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(directory)
        .status()
        .map_err(|e| format!("无法执行 {program}: {e}"))?;

    if !status.success() {
        return Err(format!(
            "{program} 失败,退出码 {}",
            status
                .code()
                .map_or_else(|| "unknown".to_owned(), |c| c.to_string())
        ));
    }
    Ok(())
}

/// 在仓库根下跑一条命令并捕获其标准输出。
pub fn capture(
    program: &str,
    args: &[&str],
) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo_root())
        .output()
        .map_err(|e| format!("无法执行 {program}: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "{program} 失败:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|e| format!("{program} 的输出不是 UTF-8: {e}"))
}

/// 读取必需的环境变量。
pub fn require_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| {
        format!(
            "环境变量 {name} 未设置 —— 请在 `nix-shell Android.nix` 或 builder 容器内运行"
        )
    })
}

/// 把产物所有权交还给宿主机用户。
///
/// 只在 Docker 路径上有意义:bind mount 的构建以 root 身份运行。
/// ponytail: 直接调 `chown`,不为此引入 libc/nix 依赖。
pub fn chown_to_host(paths: &[PathBuf]) -> Result<(), String> {
    let Ok(uid) = std::env::var("CHOWN_UID") else {
        return Ok(());
    };
    let gid =
        std::env::var("CHOWN_GID").unwrap_or_else(|_| uid.clone());
    let owner = format!("{uid}:{gid}");

    for path in paths {
        if !path.exists() {
            continue;
        }
        // 失败不致命:本机构建下压根不该走到这里。
        let _ = Command::new("chown")
            .args(["-R", &owner])
            .arg(path)
            .status();
    }
    Ok(())
}
