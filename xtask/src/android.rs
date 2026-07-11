//! `cargo xtask android` —— 交叉编译 native 库,然后让 gradle 打出 debug APK。
//!
//! 这份逻辑原先是 `scripts/build-apk.sh`。搬到 Rust 里换来两处正确性:
//! ABI → target triple 的映射有编译期穷尽性检查,platform jar 的版本排序是
//! 数值比较而非 `ls | sort -V | tail -1`。其余部分依然是"调外部工具 + 拷文件"。

use std::fs;
use std::path::{Path, PathBuf};

use crate::shell::{
    chown_to_host, repo_root, require_env, run, run_in,
};

/// gradle 工程所在。
const GRADLE_PROJECT: &str = "apps/android/gradle";
/// cargo-ndk 把 `.so` 放进来,gradle 从这里打包。
const JNI_LIBS: &str =
    "apps/android/gradle/app/src/main/jniLibs";
/// 最低支持的 Android API level。与 gradle 的 minSdk 保持一致。
const MIN_SDK: &str = "26";
/// 产物路径。
const OUTPUT_APK: &str = "dist/slint-study-debug.apk";

/// 一个 Android ABI,以及它对应的 Rust target triple。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abi {
    Arm64V8a,
    ArmeabiV7a,
    X86_64,
}

impl Abi {
    /// 解析 gradle / cargo-ndk 使用的 ABI 名。
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "arm64-v8a" => Ok(Self::Arm64V8a),
            "armeabi-v7a" => Ok(Self::ArmeabiV7a),
            "x86_64" => Ok(Self::X86_64),
            other => Err(format!("不支持的 ABI: {other}")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Arm64V8a => "arm64-v8a",
            Self::ArmeabiV7a => "armeabi-v7a",
            Self::X86_64 => "x86_64",
        }
    }

    /// NDK sysroot 里存放该 ABI 运行时库的目录名。
    fn triple(self) -> &'static str {
        match self {
            Self::Arm64V8a => "aarch64-linux-android",
            Self::ArmeabiV7a => "arm-linux-androideabi",
            Self::X86_64 => "x86_64-linux-android",
        }
    }
}

/// 入口。
pub fn build_apk(args: &[String]) -> Result<(), String> {
    let abis = parse_abis(args)?;
    let root = repo_root();

    let android_home = require_env("ANDROID_HOME")?;
    let ndk_home = require_env("ANDROID_NDK_HOME")?;

    build_native_libs(root, &abis, &android_home)?;
    copy_cxx_shared(root, &abis, Path::new(&ndk_home))?;
    assemble_debug(root, &abis)?;
    let apk = collect_artifact(root)?;

    chown_to_host(&[
        root.join("dist"),
        root.join(JNI_LIBS),
        root.join(GRADLE_PROJECT).join("app/build"),
        root.join(GRADLE_PROJECT).join(".gradle"),
    ])?;

    println!("==> Done: {}", apk.display());
    Ok(())
}

/// `--abis "arm64-v8a x86_64"`,否则读 `ABIS`,否则只构建 arm64-v8a。
fn parse_abis(args: &[String]) -> Result<Vec<Abi>, String> {
    let raw = match args {
        [] => std::env::var("ABIS")
            .unwrap_or_else(|_| "arm64-v8a".to_owned()),
        [flag, value] if flag == "--abis" => value.clone(),
        _ => {
            return Err(
                "用法: cargo xtask android [--abis \"<abi> ...\"]"
                    .to_owned(),
            );
        }
    };
    raw.split_whitespace().map(Abi::parse).collect()
}

/// 无论 APK 是哪个变体,native 库一律用 release profile:debug profile 的
/// Slint+Skia 构建体积巨大且很慢。
fn build_native_libs(
    root: &Path,
    abis: &[Abi],
    android_home: &str,
) -> Result<(), String> {
    println!(
        "==> Building Rust native libs (release) for: {}",
        abis.iter()
            .map(|a| a.name())
            .collect::<Vec<_>>()
            .join(" ")
    );

    let jni_libs = root.join(JNI_LIBS);
    if jni_libs.exists() {
        fs::remove_dir_all(&jni_libs).map_err(|e| {
            format!("无法清理 {}: {e}", jni_libs.display())
        })?;
    }

    // Slint 的 android 后端在构建时会编译一个小的 Java helper 并以 dex 形式
    // 内嵌;让它针对已安装的最新 platform jar 编译(类似 gradle 的 compileSdk)。
    if let Some(jar) = newest_android_jar(Path::new(android_home))
    {
        println!("    using ANDROID_JAR={}", jar.display());
        // SAFETY: 单线程,尚未 spawn 任何子进程。
        unsafe { std::env::set_var("ANDROID_JAR", &jar) };
    }

    // 可选透传 cargo features(仿照 ABIS 用环境变量,避免动 build_apk 的参数解析)。
    // 例:`FEATURES=bevy-3d cargo xtask android` 出带 3D 的 APK。
    let features = std::env::var("FEATURES").ok();

    // 走 `cargo ndk` 而非直接调 `cargo-ndk`:cargo 子命令期望 argv[1] 是子命令名。
    let mut args: Vec<&str> = vec!["ndk"];
    for abi in abis {
        args.push("-t");
        args.push(abi.name());
    }
    args.extend([
        "--platform",
        MIN_SDK,
        "-o",
        JNI_LIBS,
        "build",
        // default-members 不含 app-android,必须显式指定。见 docs/adr/0003。
        "-p",
        "app-android",
        "--lib",
        "--release",
    ]);
    if let Some(features) = features.as_deref() {
        args.push("--features");
        args.push(features);
    }
    run("cargo", &args)
}

/// 找出 `platforms/android-<N>/android.jar` 里 N 最大的那个。
///
/// 按数值比较,而不是字典序 —— `android-9` 不该排在 `android-34` 后面。
fn newest_android_jar(android_home: &Path) -> Option<PathBuf> {
    let platforms = android_home.join("platforms");
    let entries = fs::read_dir(platforms).ok()?;

    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let level: u32 = path
                .file_name()?
                .to_str()?
                .strip_prefix("android-")?
                .parse()
                .ok()?;
            let jar = path.join("android.jar");
            jar.is_file().then_some((level, jar))
        })
        .max_by_key(|(level, _)| *level)
        .map(|(_, jar)| jar)
}

/// Skia(Slint 的 Android 渲染器)链接的是共享版 C++ STL,必须一起打包。
fn copy_cxx_shared(
    root: &Path,
    abis: &[Abi],
    ndk_home: &Path,
) -> Result<(), String> {
    // ponytail: 写死 linux-x86_64 —— 与原 shell 脚本一致。宿主机若是 macOS,
    // 这里应当是 darwin-x86_64;等真有人在 mac 上打 APK 时再说。
    let sysroot = ndk_home
        .join("toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib");

    for abi in abis {
        let source =
            sysroot.join(abi.triple()).join("libc++_shared.so");
        if !source.is_file() {
            continue;
        }
        let destination = root
            .join(JNI_LIBS)
            .join(abi.name())
            .join("libc++_shared.so");
        if destination.exists() {
            continue;
        }
        fs::copy(&source, &destination).map_err(|e| {
            format!(
                "无法拷贝 {} -> {}: {e}",
                source.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

/// 让 gradle 把 `.so` 和 Java 存根打成 APK。
fn assemble_debug(
    root: &Path,
    abis: &[Abi],
) -> Result<(), String> {
    println!("==> Building debug APK");
    let filter = format!(
        "-PstudyAbis={}",
        abis.iter()
            .map(|a| a.name())
            .collect::<Vec<_>>()
            .join(",")
    );
    run_in(
        &root.join(GRADLE_PROJECT),
        "gradle",
        &["--no-daemon", &filter, "assembleDebug"],
    )
}

/// 把 gradle 的产物拷到 `dist/`。
fn collect_artifact(root: &Path) -> Result<PathBuf, String> {
    let source = root
        .join(GRADLE_PROJECT)
        .join("app/build/outputs/apk/debug/app-debug.apk");
    let destination = root.join(OUTPUT_APK);

    let dist = destination.parent().expect("OUTPUT_APK 必须有父目录");
    fs::create_dir_all(dist).map_err(|e| {
        format!("无法创建 {}: {e}", dist.display())
    })?;
    fs::copy(&source, &destination).map_err(|e| {
        format!("无法拷贝 {}: {e}", source.display())
    })?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个受支持的 ABI 都能解析,且映射到正确的 target triple。
    #[test]
    fn abi_解析并映射到_triple() {
        assert_eq!(
            Abi::parse("arm64-v8a").unwrap().triple(),
            "aarch64-linux-android"
        );
        assert_eq!(
            Abi::parse("armeabi-v7a").unwrap().triple(),
            "arm-linux-androideabi"
        );
        assert_eq!(
            Abi::parse("x86_64").unwrap().triple(),
            "x86_64-linux-android"
        );
    }

    /// 不认识的 ABI 必须报错,而不是静默产出一个装不上的 APK。
    #[test]
    fn 未知_abi_报错() {
        assert!(Abi::parse("mips").is_err());
    }

    /// 边界:`--abis` 缺参数、多参数都应当报错而不是被当成 ABI 名。
    #[test]
    fn abis_参数格式错误时报错() {
        assert!(
            parse_abis(&["--abis".to_owned()]).is_err()
        );
        assert!(
            parse_abis(&["x86_64".to_owned()]).is_err()
        );
    }

    /// platform jar 按 API level 的**数值**排序:android-9 不该赢过 android-34。
    #[test]
    fn 选出_api_level_最大的_platform_jar() {
        let temp = std::env::temp_dir()
            .join("xtask-newest-android-jar");
        let _ = fs::remove_dir_all(&temp);
        for level in ["9", "28", "34"] {
            let dir = temp
                .join("platforms")
                .join(format!("android-{level}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("android.jar"), b"").unwrap();
        }

        let jar = newest_android_jar(&temp).unwrap();
        assert!(
            jar.to_str().unwrap().contains("android-34"),
            "选中了 {}",
            jar.display()
        );
        let _ = fs::remove_dir_all(&temp);
    }
}
