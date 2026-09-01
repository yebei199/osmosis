use similar_asserts::assert_eq;

use super::*;

/// 在系统临时目录下开一个干净的 fixture 目录,重跑不受上一次残留影响。
fn fixture_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("xtask-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)
        .expect("建不出临时 fixture 目录");
    dir
}

/// 三个受支持的 ABI 都能解析,且映射到正确的 target triple。
#[test]
fn abi_parses_and_maps_to_triple() {
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
fn unknown_abi_errors() {
    assert!(Abi::parse("mips").is_err());
}

/// 边界:`--abis` 缺参数、多参数都应当报错而不是被当成 ABI 名。
#[test]
fn abis_flag_rejects_malformed_args() {
    assert!(parse_abis(&["--abis".to_owned()]).is_err());
    assert!(parse_abis(&["x86_64".to_owned()]).is_err());
}

/// platform jar 按 API level 的**数值**排序:android-9 不该赢过 android-34。
#[test]
fn picks_platform_jar_with_highest_api_level() {
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

/// `justfile` 的 `apk :=` 必须与 `OUTPUT_APK` 指同一个文件。
///
/// 两边各写一份字面量,谁也读不到谁 —— just 读不到 Rust 常量,Rust 也不该在
/// 构建期去解析 justfile。改了一处忘另一处时,`just android-install` 不会报错:
/// 它安静地装上 `dist/` 里上一次构建留下的旧包。这条测试把那次静默变成红字。
#[test]
fn justfile_apk_path_matches_output_apk() {
    let justfile =
        fs::read_to_string(repo_root().join("justfile"))
            .expect("读不到仓库根的 justfile");
    let declared = justfile
        .lines()
        .find_map(|line| line.strip_prefix("apk := "))
        .expect("justfile 里找不到 `apk := ` 开头的行")
        .trim()
        .trim_matches('"');

    assert_eq!(
        declared, OUTPUT_APK,
        "justfile 的 apk 与 xtask 的 OUTPUT_APK 不一致,\
         改名时漏了一处"
    );
}

/// justfile 里 `adb logcat -s` 的标签必须等于 android cdylib 的 `[lib] name`。
///
/// `apps/android/src/lib.rs` 用 `env!("CARGO_CRATE_NAME")` 取标签,改 `[lib] name`
/// 时它自动跟着变,而 justfile 里那个词不会。分家的后果是 `just android-run`
/// 一行日志都不出 —— 没有报错,看起来就像应用压根没启动。
#[test]
fn justfile_logcat_tag_matches_android_lib_name() {
    let cargo_toml = fs::read_to_string(
        repo_root().join("apps/android/Cargo.toml"),
    )
    .expect("读不到 apps/android/Cargo.toml");
    let lib_name = cargo_toml
        .split("[lib]")
        .nth(1)
        .and_then(|section| {
            section
                .lines()
                .find_map(|l| l.strip_prefix("name = "))
        })
        .expect(
            "apps/android/Cargo.toml 的 [lib] 里没有 name",
        )
        .trim()
        .trim_matches('"');

    let justfile =
        fs::read_to_string(repo_root().join("justfile"))
            .expect("读不到仓库根的 justfile");
    // 反引号、行尾等非标识符字符都不属于标签本身。
    let tags: Vec<&str> = justfile
        .match_indices("logcat -s ")
        .filter_map(|(at, needle)| {
            justfile[at + needle.len()..]
                .split_whitespace()
                .next()
        })
        .map(|tag| {
            tag.trim_matches(|c: char| {
                !(c.is_alphanumeric() || c == '_')
            })
        })
        .collect();

    assert!(
        !tags.is_empty(),
        "justfile 里找不到 `logcat -s`,这条测试已失去意义"
    );
    for tag in tags {
        assert_eq!(
            tag, lib_name,
            "justfile 的 logcat 标签与 [lib] name 不一致"
        );
    }
}

/// release 档必须点名每一个 ABI,并且真的带上 `--release`。
///
/// 这两处漏了都不会报错,只会安静地出一个坏包:少一条 `-t`,那个 ABI 的
/// `libapp_android.so` 压根不进 APK,装到对应机型上是启动即崩(找不到 so),
/// 而 gradle 那一步照样打包成功;丢了 `--release` 则出的是 debug 档,
/// Slint+Skia 的 debug 产物大到装机才发现不对。
///
/// `-p app-android --lib` 一并钉死:default-members 不含 app-android(ADR-0003),
/// 少了 `-p` 就变成编整个 workspace。
#[test]
fn release_build_names_every_abi_and_the_release_flag() {
    let args = native_build_args(
        &[Abi::Arm64V8a, Abi::X86_64],
        "release",
        None,
    );

    assert_eq!(
        args,
        vec![
            "ndk",
            "-t",
            "arm64-v8a",
            "-t",
            "x86_64",
            "--platform",
            MIN_SDK,
            "-o",
            JNI_LIBS,
            "build",
            "-p",
            "app-android",
            "--lib",
            "--release",
        ],
        "cargo ndk 的命令行与预期不符"
    );
}

/// `PROFILE=debug` 时不能再追加 `--release`。
///
/// debug 档存在的唯一理由是让 slint 生成元素调试信息(见 crates/ui/build.rs),
/// 那是 MCP 元素树和 `ElementHandle` 的前提。若这里仍然补上 `--release`,
/// 装到手机上的还是查不到任何元素的包 —— 命令行看起来一切正常,
/// 只有到了要驱动界面那一步才发现只能靠量坐标。
#[test]
fn debug_profile_drops_the_release_flag() {
    let args =
        native_build_args(&[Abi::Arm64V8a], "debug", None);

    assert!(
        !args.contains(&"--release"),
        "PROFILE=debug 却带上了 --release:{args:?}"
    );
    assert_eq!(
        args.last().copied(),
        Some("--lib"),
        "debug 档的命令行应当止于 --lib"
    );
}

/// `FEATURES` 只在真给了值时才透传,而且必须是 `--features <值>` 一对。
///
/// 没设 FEATURES 时若照样推一个 `--features` 出去,cargo 会把它后面的东西
/// (或什么都没有)当成 feature 名,报出一句与 FEATURES 毫无关系的错。
#[test]
fn features_are_appended_only_when_asked() {
    let without = native_build_args(
        &[Abi::Arm64V8a],
        "release",
        None,
    );
    assert!(
        !without.contains(&"--features"),
        "没设 FEATURES 却推了 --features:{without:?}"
    );

    // 值本身对这个函数是不透明的,它只负责原样递给 cargo。
    let with = native_build_args(
        &[Abi::Arm64V8a],
        "release",
        Some("any-feature-name"),
    );
    assert_eq!(
        &with[with.len() - 2..],
        ["--features", "any-feature-name"],
        "FEATURES 没有作为一对参数透传过去"
    );
}

/// 上一轮构建留下的其它 ABI 的 `.so` 必须被清掉。
///
/// cargo-ndk 只写这一轮点名的 ABI,而 gradle 是照单全收地打包整个 jniLibs。
/// 不清理的话,`--abis arm64-v8a` 出的包里会混进上次那个 x86_64 的库:
/// 包白白变大,里面那份代码还是旧的,而整条流水线一句警告都不会给。
#[test]
fn clear_jni_libs_removes_a_previous_abis_libraries() {
    let root = fixture_dir("clear-jni-libs-stale");
    let stale = root.join(JNI_LIBS).join("x86_64");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("libapp_android.so"), b"old")
        .unwrap();

    clear_jni_libs(&root).expect("清理 jniLibs 不该失败");

    assert!(
        !root.join(JNI_LIBS).exists(),
        "jniLibs 还在,上一轮的 .so 会被打进这次的包"
    );
    let _ = fs::remove_dir_all(&root);
}

/// jniLibs 还不存在时清理不算错。
///
/// 刚 clone 的机器上那个目录本来就没有。这里若把 `remove_dir_all` 的 NotFound
/// 当失败往上抛,第一次 `cargo xtask android` 会直接死在清理这一步,
/// 报的还是"无法清理"这种看不出所以然的话。
#[test]
fn clear_jni_libs_accepts_a_missing_directory() {
    let root = fixture_dir("clear-jni-libs-missing");

    assert_eq!(
        clear_jni_libs(&root),
        Ok(()),
        "目录不存在时不该报错"
    );
    let _ = fs::remove_dir_all(&root);
}

/// `--abis` 写坏时必须在碰工具链之前就退出。
///
/// 参数校验排在读 `ANDROID_HOME` 和调 cargo-ndk 前面,否则用户要等上几分钟的
/// 交叉编译,才等来一句"不支持的 ABI"。这条同时保证坏参数不会被静默忽略 ——
/// 忽略掉的话出的是默认 arm64-v8a 的包,而用户以为自己点的是别的 ABI。
#[test]
fn build_apk_rejects_bad_abis_before_the_toolchain() {
    let error = build_apk(&["--abis".to_owned()])
        .expect_err("`--abis` 缺值时不该继续往下走");

    assert!(
        error.starts_with("用法:"),
        "报的不是用法错误,说明已经走到工具链那一步:{error}"
    );
}
