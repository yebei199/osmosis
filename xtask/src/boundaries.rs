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

    run_checks(&checks)
}

/// 跑完每一条,把所有失败一次性报出来,而不是遇到第一个就退出。
///
/// 与检查表分开,是因为这条聚合规则本身值得单独测:每一条真检查都要跑
/// `cargo tree` 或 `cargo check --target wasm32`,在单测里跑不动,
/// 而"是不是真的跑完了每一条"恰恰是这里唯一的逻辑。
fn run_checks(
    checks: &[(&str, Check)],
) -> Result<(), String> {
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

/// 3D 桥(render3d/bevy/wgpu)完全不进 web / ios。
///
/// bevy 在桌面与 android 上是硬依赖(docs/adr/0011),但 web / ios 一旦拉进 bevy
/// 或 wgpu,体积爆炸;这两端按「余端 graceful 缺省」退回无 3D 形态 —— 空图缺省、
/// 锚点恒无,`.slint` 里零平台判断。曾经三端各有一个 opt-in 的 `bevy-3d` feature,
/// 随 0011 一并拆除(可关性只剩一个没人验证过的降级形态);哪天要给 web / ios
/// 开 3D,是把 render3d 接进它们的入口 crate,而不是复活那个 feature。
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
    let upstream = repo_root()
        .join(&repo)
        .join(UPSTREAM_PROTO_IN_REPO);
    if !upstream.exists() {
        return Err(format!(
            "{UPSTREAM_REPO_ENV}={repo} 下没有 {UPSTREAM_PROTO_IN_REPO} —— 指错工作树了?"
        ));
    }

    proto_drift(
        &repo_root().join(VENDORED_PROTO),
        &upstream,
    )
}

/// 比对副本与上游那两份 `.proto`,一致返回 `Ok`,分歧则说清是第几行。
///
/// 与上面那层分开,是为了让"读两份文件、报第几行分歧"这段能对着临时目录里的
/// fixture 测 —— 外层那层依赖 `BANG_DREAM_REPO` 指向一个真实的上游工作树,
/// 只有装了那个仓库的机器上才跑得动。
fn proto_drift(
    vendored: &Path,
    upstream: &Path,
) -> Result<(), String> {
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|error| {
            format!("读不到 {}:{error}", path.display())
        })
    };

    match first_difference(
        &read(vendored)?,
        &read(upstream)?,
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
    use std::path::PathBuf;

    use similar_asserts::assert_eq;

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

    fn passing_check() -> Result<(), String> {
        Ok(())
    }

    fn contract_check_fails() -> Result<(), String> {
        Err("contract 依赖了 tokio".to_owned())
    }

    fn wasm_check_fails() -> Result<(), String> {
        Err("app-web 依赖了 cpal".to_owned())
    }

    /// 一轮必须跑完每一条,把所有失败一起报出来。
    ///
    /// 遇到第一条失败就退出的话,`just ci` 一轮只暴露一个问题:修完 contract
    /// 那条再跑,才知道 app-web 那条也是红的。边界检查一次跑十几分钟
    /// (每条都要 `cargo tree` 或 wasm 的 `cargo check`),这种一次只给一个
    /// 答案的循环代价很高。
    #[test]
    fn run_checks_reports_every_failure_not_just_the_first()
    {
        let checks: [(&str, Check); 3] = [
            ("contract 只依赖 serde", contract_check_fails),
            ("api 在 wasm 上不依赖 tokio", passing_check),
            ("web 不依赖原生音频", wasm_check_fails),
        ];

        let error = run_checks(&checks)
            .expect_err("有两条失败,不该返回 Ok");

        assert!(
            error.contains("contract 依赖了 tokio"),
            "第一条失败的原因丢了:{error}"
        );
        assert!(
            error.contains("app-web 依赖了 cpal"),
            "遇到第一条失败就停了,后面那条没跑:{error}"
        );
        assert!(
            !error.contains("api 在 wasm 上不依赖 tokio"),
            "通过的那条不该出现在失败清单里:{error}"
        );
    }

    /// 全过时必须是 `Ok`,而不是一份空的失败清单。
    ///
    /// 若拿 `failures` 是否为空这件事判错了方向,`just ci` 会在一切正常时
    /// 报"架构边界被破坏:"后面跟一片空白 —— 谁也不知道该改哪里。
    #[test]
    fn run_checks_passes_when_every_check_passes() {
        let checks: [(&str, Check); 2] = [
            ("contract 只依赖 serde", passing_check),
            ("api 在 wasm 上不依赖 tokio", passing_check),
        ];

        assert_eq!(
            run_checks(&checks),
            Ok(()),
            "所有检查都通过时不该报错"
        );
    }

    /// 多余的参数要报用法,不能被吞掉。
    ///
    /// `cargo xtask boundaries --fix` 这类拼出来的子命令并不存在。静默忽略的话
    /// 它跑的是普通检查、并且成功返回,用户会以为自己要的那件事已经做了。
    #[test]
    fn verify_rejects_extra_arguments() {
        let error = verify(&["--fix".to_owned()])
            .expect_err("多余参数不该被忽略");

        assert!(
            error.starts_with("用法:"),
            "报的不是用法错误:{error}"
        );
    }

    /// 临时目录里写一份 `.proto`,返回它的路径。
    fn proto_fixture(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("xtask-proto-{name}"));
        fs::create_dir_all(&dir)
            .expect("建不出临时 fixture 目录");
        let path = dir.join("music.proto");
        fs::write(&path, body).expect("写不进 fixture");
        path
    }

    /// 两份一模一样时不能报漂移。
    ///
    /// 误报的代价是这条检查会被当成噪音关掉,而它是副本与上游之间唯一的护栏。
    #[test]
    fn proto_drift_accepts_identical_copies() {
        let vendored = proto_fixture("same-a", PROTO);
        let upstream = proto_fixture("same-b", PROTO);

        assert_eq!(
            proto_drift(&vendored, &upstream),
            Ok(()),
            "两份内容相同却报了漂移"
        );
    }

    /// 分歧要指出行号,并给出把副本同步过去的那条命令。
    ///
    /// 只说一句"不一致"的话,人得自己 diff 两个仓库里的文件才知道差在哪 ——
    /// 而上游那份根本不在本仓库里,连路径都要现找。行号和 `cp` 命令是这条
    /// 检查报错时唯一有用的东西。
    #[test]
    fn proto_drift_points_at_the_line_that_diverged() {
        let vendored = proto_fixture("drift-a", PROTO);
        let upstream = proto_fixture(
            "drift-b",
            &PROTO
                .replace("string id = 1;", "int64 id = 1;"),
        );

        let error = proto_drift(&vendored, &upstream)
            .expect_err("第三行不同,应当报漂移");

        assert!(
            error.contains("第 3 行"),
            "没指出分歧的行号:{error}"
        );
        assert!(
            error.contains(&format!(
                "cp {}",
                upstream.display()
            )),
            "没给出同步副本的命令:{error}"
        );
    }

    /// 读不到文件时报的是"读不到 <路径>",而不是伪装成一次内容漂移。
    ///
    /// `BANG_DREAM_REPO` 指向的工作树被挪走、或副本被误删时,若这里退化成
    /// "两份不一致",人会照着提示去 diff 一个根本不存在的文件。
    #[test]
    fn proto_drift_names_the_file_it_could_not_read() {
        let vendored = proto_fixture("missing-a", PROTO);
        let absent =
            vendored.with_file_name("nowhere.proto");

        let error = proto_drift(&vendored, &absent)
            .expect_err("上游那份不存在,应当报错");

        assert!(
            error.starts_with("读不到 "),
            "报的不是读文件失败:{error}"
        );
        assert!(
            error.contains("nowhere.proto"),
            "没说清读不到的是哪一个文件:{error}"
        );
    }
}
