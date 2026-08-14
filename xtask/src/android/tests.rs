use super::*;

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
