//! `cargo xtask boundaries` —— 把架构决策变成可执行的断言。
//!
//! ADR 里写的约束靠记忆是守不住的。这些检查在 CI 与本地(`just ci`)跑的是
//! **同一份代码**,不是两份互相漂移的 shell 片段。

use std::env;
use std::fs;
use std::path::Path;

use crate::shell::{capture, repo_root, run};

/// codegen 实际读的那一份(`server/build.rs`)。
const VENDORED_PROTO: &str =
    "server/proto/music/v1/music.proto";
/// 上游 bang-dream 工作树的位置。它是独立仓库,不在本仓库里。
const UPSTREAM_REPO_ENV: &str = "BANG_DREAM_REPO";
/// 上游那份契约在它自己仓库里的相对位置。
const UPSTREAM_PROTO_IN_REPO: &str =
    "proto/music/v1/music.proto";

/// `contract` 的依赖白名单之外的东西。
///
/// 只要有一个混进来,web 端就编不过 —— 而且那个错误只会在别人下次构建 wasm 时
/// 才炸,与引入它的那次提交隔着好几天。见 `docs/adr/0001`。
const FORBIDDEN_IN_CONTRACT: &[&str] =
    &["tokio", "sqlx", "reqwest", "axum", "hyper"];

/// 一条边界检查:通过返回 `Ok`,否则给出人话解释。
type Check = fn() -> Result<(), String>;

/// 逐条执行,把所有失败一次性报出来,而不是遇到第一个就退出。
pub fn verify(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(
            "用法: cargo xtask boundaries".to_owned()
        );
    }

    let checks: [(&str, Check); 6] = [
        (
            "contract 只依赖 serde",
            contract_has_no_io_crates,
        ),
        (
            "api 在 wasm 上不依赖 tokio",
            api_is_tokio_free_on_wasm,
        ),
        (
            "app-core 能编到 wasm",
            app_core_compiles_for_wasm,
        ),
        ("web/ios 不依赖 bevy/wgpu", web_ios_free_of_3d),
        (
            "web 不依赖 audio/syncplay 的原生栈",
            web_free_of_native_audio,
        ),
        (
            "vendored .proto 与上游一致",
            vendored_proto_matches_upstream,
        ),
    ];

    let mut failures = Vec::new();
    for (name, check) in checks {
        match check() {
            Ok(()) => println!("  ok    {name}"),
            Err(message) => {
                println!("  FAIL  {name}");
                failures.push(format!("{name}: {message}"));
            }
        }
    }

    if failures.is_empty() {
        return Ok(());
    }
    Err(format!(
        "架构边界被破坏:\n  {}",
        failures.join("\n  ")
    ))
}

/// ADR-0001:契约只共享线上格式,不共享 IO。
fn contract_has_no_io_crates() -> Result<(), String> {
    let tree = capture(
        "cargo",
        &["tree", "-p", "contract", "--edges", "normal"],
    )?;

    let found: Vec<&str> = FORBIDDEN_IN_CONTRACT
        .iter()
        .copied()
        .filter(|forbidden| depends_on(&tree, forbidden))
        .collect();

    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "contract 依赖了 {},违反 docs/adr/0001",
        found.join("、")
    ))
}

/// ADR-0002:`Send` 边界关在 api 内部,wasm 上不该出现 tokio。
fn api_is_tokio_free_on_wasm() -> Result<(), String> {
    let tree = capture(
        "cargo",
        &[
            "tree",
            "-p",
            "api",
            "--target",
            "wasm32-unknown-unknown",
            "--edges",
            "normal",
        ],
    )?;

    if !depends_on(&tree, "tokio") {
        return Ok(());
    }
    Err("api 在 wasm 上依赖了 tokio,违反 docs/adr/0002"
        .to_owned())
}

/// ADR-0002:app-core 的 future 不要求 `Send`,因此能原样编到 wasm。
fn app_core_compiles_for_wasm() -> Result<(), String> {
    run(
        "cargo",
        &[
            "check",
            "--quiet",
            "-p",
            "app-core",
            "--target",
            "wasm32-unknown-unknown",
        ],
    )
}

/// 3D 桥(render3d/bevy/wgpu)不进 web / ios 的**默认**构建。
///
/// web / ios 的默认产物一旦拉进 bevy 或 wgpu,体积爆炸,且违反「余端 graceful 缺省」
/// 的约定(见计划 `bevy-serialized-dove`):默认应隐藏 3D 面板,而非把整套渲染器
/// 打包进去。web 自 slint#11580 起有了 opt-in 的 `bevy-3d` feature(desktop/android
/// 同名),但 `cargo tree` 查的是默认 feature 集,这里守住的正是「默认不外溢」;
/// ios 则仍然完全禁入。
fn web_ios_free_of_3d() -> Result<(), String> {
    const FORBIDDEN: &[&str] =
        &["render3d", "bevy", "wgpu"];

    for pkg in ["app-web", "app-ios"] {
        let tree = capture(
            "cargo",
            &["tree", "-p", pkg, "--edges", "normal"],
        )?;
        let found: Vec<&str> = FORBIDDEN
            .iter()
            .copied()
            .filter(|forbidden| {
                depends_on(&tree, forbidden)
            })
            .collect();
        if !found.is_empty() {
            return Err(format!(
                "{pkg} 依赖了 {},3D 桥不该进 web/ios",
                found.join("、")
            ));
        }
    }
    Ok(())
}

/// 原生音频与 WebRTC 栈(audio/rodio/cpal、syncplay/webrtc)不进 web。
///
/// cpal 在 linux 上链接 alsa、在 android 上链接 AAudio —— 两者在 wasm 上都不存在。
/// 混进去的话 web 端直接编不过,而那个错误只会在别人下次构建 wasm 时炸出来,
/// 离引入它的那次改动已经很远。web 将来要出声得走 WebAudio,是另一套实现,
/// 差异吸收在 `audio` crate 内部,与 `docs/adr/0002` 同一个模式。
///
/// ios 不在此列:它有 CoreAudio,cpal 支持它,只是本项目还没实现那个端。
fn web_free_of_native_audio() -> Result<(), String> {
    const FORBIDDEN: &[&str] =
        &["audio", "rodio", "cpal", "syncplay", "webrtc"];

    // 必须带 `--target`:`audio` 是 ui 的 `cfg(not(wasm32))` 条件依赖,
    // 不指定目标时 cargo tree 按宿主算,这条规则会对着**正确的**代码报红。
    // 3D 那条不带 --target 是因为它守的是「默认 feature 集不外溢」,不是同一件事。
    let tree = capture(
        "cargo",
        &[
            "tree",
            "-p",
            "app-web",
            "--target",
            "wasm32-unknown-unknown",
            "--edges",
            "normal",
        ],
    )?;
    let found: Vec<&str> = FORBIDDEN
        .iter()
        .copied()
        .filter(|forbidden| depends_on(&tree, forbidden))
        .collect();
    if !found.is_empty() {
        return Err(format!(
            "app-web 依赖了 {},这些原生栈编不到 wasm",
            found.join("、")
        ));
    }
    Ok(())
}

/// 契约的上游是 bang-dream,那是一个独立仓库,不以任何形式挂在本仓库里。
/// `server/proto` 存的是它的副本,codegen 只读副本,CI 因此不需要跨仓库凭据。
/// 代价是两份可能漂移,这条检查就是兜住漂移的地方 —— `BANG_DREAM_REPO` 指向上游
/// 工作树时比对,没指(CI、以及不碰契约的人)时跳过。
fn vendored_proto_matches_upstream() -> Result<(), String> {
    let Ok(repo) = env::var(UPSTREAM_REPO_ENV) else {
        println!(
            "        (跳过:{UPSTREAM_REPO_ENV} 未设置,拿不到上游)"
        );
        return Ok(());
    };

    // 相对路径按仓库根解释,与 justfile 的 `cd {{repo}}` 一致;绝对路径原样生效。
    let upstream =
        repo_root().join(&repo).join(UPSTREAM_PROTO_IN_REPO);
    if !upstream.exists() {
        return Err(format!(
            "{UPSTREAM_REPO_ENV}={repo} 下没有 {UPSTREAM_PROTO_IN_REPO} —— 指错工作树了?"
        ));
    }

    let vendored = repo_root().join(VENDORED_PROTO);
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|error| {
            format!("读不到 {}:{error}", path.display())
        })
    };

    match first_difference(
        &read(&vendored)?,
        &read(&upstream)?,
    ) {
        None => Ok(()),
        Some(line) => Err(format!(
            "{VENDORED_PROTO} 与 {} 在第 {line} 行起分歧 —— \
             上游改了契约就把副本同步过去:cp {} {VENDORED_PROTO}",
            upstream.display(),
            upstream.display()
        )),
    }
}

/// 两份文本第一处分歧的 1-based 行号,完全一致时为 `None`。
///
/// 逐行比,不做 proto 的语义解析:副本是 codegen 的唯一输入,连注释差异都值得看一眼。
/// 一侧是另一侧前缀时,分歧记在长的那侧多出来的第一行。
fn first_difference(
    left: &str,
    right: &str,
) -> Option<usize> {
    let common = left
        .lines()
        .zip(right.lines())
        .position(|(a, b)| a != b);
    if let Some(index) = common {
        return Some(index + 1);
    }

    let (left_lines, right_lines) =
        (left.lines().count(), right.lines().count());
    (left_lines != right_lines)
        .then(|| left_lines.min(right_lines) + 1)
}

/// `cargo tree` 的输出里是否出现了名为 `name` 的 crate。
///
/// 按**词**比较,不是子串:`tokio-util` 不算 `tokio`,`hyper-util` 不算 `hyper`。
/// 树形符号(`├──` 等)本身就是独立的空白分隔词,不会干扰。
fn depends_on(tree: &str, name: &str) -> bool {
    tree.lines().any(|line| {
        line.split_whitespace().any(|word| word == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "\
api v0.1.0 (/repo/crates/api)
├── contract v0.1.0 (/repo/crates/contract)
│   └── serde v1.0.228
├── reqwest v0.12.28
│   └── hyper-util v0.1.20
└── tokio-util v0.7.0";

    /// 直接出现在树里的 crate 会被认出来。
    #[test]
    fn depends_on_detects_direct_dependency() {
        assert!(depends_on(TREE, "reqwest"));
        assert!(depends_on(TREE, "serde"));
    }

    /// 边界:名字是另一个 crate 的前缀时不能误报。
    /// `tokio-util` 不是 `tokio`,`hyper-util` 不是 `hyper`。
    #[test]
    fn depends_on_rejects_prefix_match() {
        assert!(!depends_on(TREE, "tokio"));
        assert!(!depends_on(TREE, "hyper"));
    }

    /// 边界:空树、不存在的名字。
    #[test]
    fn depends_on_handles_empty_input() {
        assert!(!depends_on("", "tokio"));
        assert!(!depends_on(TREE, "sqlx"));
    }

    const PROTO: &str = "\
syntax = \"proto3\";
package bangdream.music.v1;
message Track { string id = 1; }";

    /// 两份完全一致时没有分歧行。
    #[test]
    fn first_difference_accepts_identical_input() {
        assert_eq!(first_difference(PROTO, PROTO), None);
        assert_eq!(first_difference("", ""), None);
    }

    /// 中间某行改了,报的是那一行的 1-based 行号。
    #[test]
    fn first_difference_reports_changed_line() {
        let changed = PROTO
            .replace("string id = 1;", "int64 id = 1;");
        assert_eq!(
            first_difference(PROTO, &changed),
            Some(3)
        );
    }

    /// 边界:一侧是另一侧的前缀。公共部分逐行相同,分歧在第一行多出来的地方。
    #[test]
    fn first_difference_reports_appended_line() {
        let longer = format!("{PROTO}\nmessage Album {{}}");
        assert_eq!(
            first_difference(PROTO, &longer),
            Some(4)
        );
        assert_eq!(
            first_difference(&longer, PROTO),
            Some(4)
        );
    }

    /// 边界:一侧为空。
    #[test]
    fn first_difference_reports_empty_side() {
        assert_eq!(first_difference("", PROTO), Some(1));
        assert_eq!(first_difference(PROTO, ""), Some(1));
    }
}
