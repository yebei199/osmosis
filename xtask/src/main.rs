//! 构建逻辑。经 `.cargo/config.toml` 的 alias 暴露为 `cargo xtask <命令>`。
//!
//! **xtask 只管编译逻辑,不管工具链供给。** 它是一个普通的 Rust 程序,
//! 没法自举出自己的 Android SDK/NDK —— 那仍然是 `Android.nix` 与
//! `docker/Dockerfile` 的职责。调用形态:
//!
//! ```sh
//! nix-shell Android.nix --run 'cargo xtask android'
//! docker run ... cargo xtask android
//! ```
//!
//! 见 `docs/adr/0004`。

mod android;
mod boundaries;
mod shell;

use std::process::ExitCode;

/// 用法说明。命令一多就该考虑换成真正的参数解析库,现在还不必。
const USAGE: &str = "\
用法: cargo xtask <命令>

命令:
  android [--abis \"<abi> ...\"]   交叉编译 native 库并打出 debug APK
  boundaries                     校验 ADR 里的架构约束(CI 与 `just ci` 共用)

环境变量:
  ABIS               空格分隔的 ABI 列表(默认 arm64-v8a),被 --abis 覆盖
  CARGO_TARGET_DIR   Rust target 目录(默认 <仓库根>/target-android)
  CHOWN_UID/GID      仅 Docker 用:把产物所有权交还给宿主机用户
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let result = match command.as_str() {
        "android" => android::build_apk(&args[1..]),
        "boundaries" => boundaries::verify(&args[1..]),
        other => Err(format!("未知命令: {other}\n\n{USAGE}")),
    };

    if let Err(message) = result {
        eprintln!("error: {message}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
