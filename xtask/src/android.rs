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
const OUTPUT_APK: &str = "dist/osmosis-debug.apk";

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
///
/// **armeabi-v7a 不在默认里**,而且是有意的:`skia-bindings` 没有 armv7 的预编译
/// 产物(404),回退到全量编 skia,那条路还要 `ANDROID_NDK`,而 `Android.nix` 只导出了
/// cargo-ndk 自己那套。为一个几乎绝迹的 ABI 付一次全量 skia 构建换不回什么 ——
/// minSdk 是 26,64 位从 Android 5 就有了(issue #47)。
/// 真需要仍可 `ABIS="armeabi-v7a"` 显式要,但得先把 NDK 那一步解决掉。
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

/// native 库的 profile。默认 release:debug 档的 Slint+Skia 构建体积巨大且很慢,
/// 发布件也不需要调试信息。
///
/// `PROFILE=debug` 换成 debug 档,开发装机用 —— slint 的元素调试信息只在
/// debug 档生成(见 crates/ui/build.rs),而它是 slint-app MCP 元素树与
/// `ElementHandle` 的前提。装了 release 包的手机上,MCP 只截得到图、查不到
/// 任何元素,驱动界面就只能靠量坐标。
fn native_profile() -> Result<&'static str, String> {
    match std::env::var("PROFILE").as_deref() {
        Ok("debug") => Ok("debug"),
        Ok("release") | Err(_) => Ok("release"),
        Ok(other) => Err(format!(
            "PROFILE 只认 debug 或 release,收到 {other}"
        )),
    }
}

fn build_native_libs(
    root: &Path,
    abis: &[Abi],
    android_home: &str,
) -> Result<(), String> {
    let profile = native_profile()?;
    println!(
        "==> Building Rust native libs ({profile}) for: {}",
        abis.iter()
            .map(|a| a.name())
            .collect::<Vec<_>>()
            .join(" ")
    );

    clear_jni_libs(root)?;

    // Slint 的 android 后端在构建时会编译一个小的 Java helper 并以 dex 形式
    // 内嵌;让它针对已安装的最新 platform jar 编译(类似 gradle 的 compileSdk)。
    if let Some(jar) =
        newest_android_jar(Path::new(android_home))
    {
        println!("    using ANDROID_JAR={}", jar.display());
        // SAFETY: 单线程,尚未 spawn 任何子进程。
        unsafe { std::env::set_var("ANDROID_JAR", &jar) };
    }

    // 可选透传 cargo features(仿照 ABIS 用环境变量,避免动 build_apk 的参数解析)。
    // 例:`FEATURES=bevy-3d cargo xtask android` 出带 3D 的 APK。
    let features = std::env::var("FEATURES").ok();

    let args = native_build_args(
        abis,
        profile,
        features.as_deref(),
    );
    run("cargo", &args)
}

/// 清空 jniLibs。目录本来就不在(刚 clone 的机器)不算错。
///
/// 必须先清空再让 cargo-ndk 往里写:它只写这一轮点名的 ABI,上一轮留下的
/// 其它 ABI 的 `.so` 还躺在原地,而 gradle 是照单全收地打包整个目录。
/// 于是 `--abis arm64-v8a` 出来的包里会混进上次那个 x86_64 的库 —— 包变大,
/// 且里面那份代码是旧的。
fn clear_jni_libs(root: &Path) -> Result<(), String> {
    let jni_libs = root.join(JNI_LIBS);
    if !jni_libs.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&jni_libs).map_err(|e| {
        format!("无法清理 {}: {e}", jni_libs.display())
    })
}

/// 拼出 `cargo ndk` 的命令行。
///
/// 走 `cargo ndk` 而非直接调 `cargo-ndk`:cargo 子命令期望 argv[1] 是子命令名。
fn native_build_args<'a>(
    abis: &[Abi],
    profile: &str,
    features: Option<&'a str>,
) -> Vec<&'a str> {
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
    ]);
    if profile == "release" {
        args.push("--release");
    }
    if let Some(features) = features {
        args.push("--features");
        args.push(features);
    }
    args
}

/// 找出 `platforms/android-<N>/android.jar` 里 N 最大的那个。
///
/// 按数值比较,而不是字典序 —— `android-9` 不该排在 `android-34` 后面。
fn newest_android_jar(
    android_home: &Path,
) -> Option<PathBuf> {
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
        let source = sysroot
            .join(abi.triple())
            .join("libc++_shared.so");
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
fn collect_artifact(
    root: &Path,
) -> Result<PathBuf, String> {
    let source = root
        .join(GRADLE_PROJECT)
        .join("app/build/outputs/apk/debug/app-debug.apk");
    let destination = root.join(OUTPUT_APK);

    let dist = destination
        .parent()
        .expect("OUTPUT_APK 必须有父目录");
    fs::create_dir_all(dist).map_err(|e| {
        format!("无法创建 {}: {e}", dist.display())
    })?;
    fs::copy(&source, &destination).map_err(|e| {
        format!("无法拷贝 {}: {e}", source.display())
    })?;
    Ok(destination)
}

#[cfg(test)]
mod tests;
