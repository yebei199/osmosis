//! `cargo xtask boundaries` —— 把架构决策变成可执行的断言。
//!
//! ADR 里写的约束靠记忆是守不住的。这些检查在 CI 与本地(`just ci`)跑的是
//! **同一份代码**,不是两份互相漂移的 shell 片段。

use std::env;
use std::fs;
use std::path::Path;

use crate::shell::{capture, repo_root};

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
/// 只要有一个混进来,客户端就被拖进服务端的 IO 栈 —— 而且服务端自己的 CI 看不出来。
/// 见 `docs/adr/0001`。
const FORBIDDEN_IN_CONTRACT: &[&str] =
    &["tokio", "sqlx", "reqwest", "axum", "hyper"];

/// `app-core` 源码里不许出现的调用:时钟、线程、文件系统。
///
/// 纯规则层没有隐式的时间源和 IO,「现在几点」由调用方传进来。web 还在时这条由
/// wasm 编译顺带守着,web 废弃(#110)之后只剩这里。见 `docs/adr/0002`。
const IMPURE_IN_APP_CORE: &[&str] = &[
    "SystemTime",
    "Instant::now",
    "thread::spawn",
    "std::fs",
];

/// 全仓 `.rs` 里不许出现的调用:系统临时目录。
///
/// 拿它拼一个固定名字,同一台机器上并行的另一份测试就会删掉、覆盖这边的文件(#136)。
/// 临时目录一律走 `tempfile`(名字唯一、用完即删)。正式代码眼下一处都不用它;
/// 真要用时在这里给那个文件开豁免,而不是把检查缩回「只看测试」。
const SHARED_TEMP_DIR: &str = "temp_dir()";

/// 本文件自己要写出 [`SHARED_TEMP_DIR`] 当检查的目标与单测的输入,扫描跳过它。
const THIS_FILE: &str = "xtask/src/boundaries.rs";

/// 一条边界检查:通过返回 `Ok`,否则给出人话解释。
type Check = fn() -> Result<(), String>;

/// 逐条执行,把所有失败一次性报出来,而不是遇到第一个就退出。
pub fn verify(args: &[String]) -> Result<(), String> {
    if !args.is_empty() {
        return Err(
            "用法: cargo xtask boundaries".to_owned()
        );
    }

    let checks: [(&str, Check); 4] = [
        (
            "contract 只依赖 serde",
            contract_has_no_io_crates,
        ),
        (
            "临时目录不用整机共享的固定名字",
            no_shared_temp_dir,
        ),
        (
            "app-core 不碰时钟、线程、文件系统",
            app_core_is_pure,
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

/// ADR-0002:`app-core` 是纯规则层。逐个 `.rs` 文件扫 [`IMPURE_IN_APP_CORE`]。
fn app_core_is_pure() -> Result<(), String> {
    let mut found = Vec::new();
    scan_impure(
        &repo_root().join("crates/app-core/src"),
        &mut found,
    )?;
    if found.is_empty() {
        return Ok(());
    }
    Err(format!("{},违反 docs/adr/0002", found.join("、")))
}

fn scan_impure(
    dir: &Path,
    found: &mut Vec<String>,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| {
        format!("读不了 {}: {error}", dir.display())
    })?;
    for entry in entries {
        let path = entry
            .map_err(|error| {
                format!("读不了 {}: {error}", dir.display())
            })?
            .path();
        if path.is_dir() {
            scan_impure(&path, found)?;
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source =
            fs::read_to_string(&path).map_err(|error| {
                format!(
                    "读不了 {}: {error}",
                    path.display()
                )
            })?;
        found.extend(
            impure_calls(&source).into_iter().map(|call| {
                format!("{} 调了 {call}", path.display())
            }),
        );
    }
    Ok(())
}

/// #136:临时目录一律走 `tempfile`。扫仓库里所有未被忽略的 `.rs`,报文件与行号。
fn no_shared_temp_dir() -> Result<(), String> {
    let files = capture(
        "git",
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.rs",
        ],
    )?;

    let mut found = Vec::new();
    for file in
        files.lines().filter(|file| *file != THIS_FILE)
    {
        let path = repo_root().join(file);
        // 已删除但还没提交的文件仍在 `--cached` 里,读不到就跳过
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        found.extend(
            temp_dir_lines(&source)
                .into_iter()
                .map(|line| format!("{file}:{line}")),
        );
    }

    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} 调了 std::env::{SHARED_TEMP_DIR},改用 tempfile::tempdir()",
        found.join("、")
    ))
}

/// 源码里调了 [`SHARED_TEMP_DIR`] 的行,1-based。注释行跳过,理由同 [`impure_calls`]。
fn temp_dir_lines(source: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            !line.trim_start().starts_with("//")
                && line.contains(SHARED_TEMP_DIR)
        })
        .map(|(index, _)| index + 1)
        .collect()
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

/// 源码里出现的时钟、线程、文件系统调用,按 [`IMPURE_IN_APP_CORE`] 的顺序列出。
///
/// 注释行跳过:文档要能写「不要调 `SystemTime::now()`」。
fn impure_calls(source: &str) -> Vec<&'static str> {
    let code: Vec<&str> = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();
    IMPURE_IN_APP_CORE
        .iter()
        .copied()
        .filter(|call| {
            code.iter().any(|line| line.contains(call))
        })
        .collect()
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

    /// 时钟、线程、文件系统的调用都会被认出来。
    #[test]
    fn impure_calls_detects_clock_thread_and_fs() {
        let source = "\
let now = std::time::SystemTime::now();
let start = Instant::now();
std::thread::spawn(|| {});
let text = std::fs::read_to_string(path);";
        assert_eq!(
            impure_calls(source),
            vec![
                "SystemTime",
                "Instant::now",
                "thread::spawn",
                "std::fs"
            ]
        );
    }

    /// 边界:注释里提到这些名字不算 —— 文档要能写「不要调 `SystemTime::now()`」。
    #[test]
    fn impure_calls_ignores_comments() {
        let source = "\
//! 不碰时钟,不要写 SystemTime::now()。
    /// Instant::now 由调用方传进来。
    // std::fs 也一样
fn tick(now_ms: u64) {}";
        assert!(impure_calls(source).is_empty());
    }

    /// 边界:空输入。
    #[test]
    fn impure_calls_handles_empty_input() {
        assert!(impure_calls("").is_empty());
    }

    /// 调用会被认出来,报的是 1-based 行号;`env::temp_dir()` 这种写法也算。
    #[test]
    fn temp_dir_lines_detects_calls() {
        let source = "\
fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(\"x\");
    let other = env::temp_dir();
    dir
}";
        assert_eq!(temp_dir_lines(source), vec![2, 3]);
    }

    /// 边界:注释里提到它不算 —— 文档要能写「别用 `std::env::temp_dir()`」。
    /// `tempfile::tempdir()` 是推荐的写法,不能被误报。
    #[test]
    fn temp_dir_lines_ignores_comments_and_tempfile() {
        let source = "\
//! 别用 std::env::temp_dir()。
    /// temp_dir() 拼固定名字会被并行的测试踩。
let dir = tempfile::tempdir().unwrap();";
        assert!(temp_dir_lines(source).is_empty());
    }

    /// 边界:空输入。
    #[test]
    fn temp_dir_lines_handles_empty_input() {
        assert!(temp_dir_lines("").is_empty());
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

    /// 在 `dir` 里写一份名为 `name` 的 `.proto`,返回它的路径。
    fn proto_fixture(
        dir: &tempfile::TempDir,
        name: &str,
        body: &str,
    ) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, body).expect("写不进 fixture");
        path
    }

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir()
            .expect("建不出临时 fixture 目录")
    }

    /// 两份一模一样时不能报漂移。
    ///
    /// 误报的代价是这条检查会被当成噪音关掉,而它是副本与上游之间唯一的护栏。
    #[test]
    fn proto_drift_accepts_identical_copies() {
        let dir = scratch();
        let vendored =
            proto_fixture(&dir, "a.proto", PROTO);
        let upstream =
            proto_fixture(&dir, "b.proto", PROTO);

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
        let dir = scratch();
        let vendored =
            proto_fixture(&dir, "a.proto", PROTO);
        let upstream = proto_fixture(
            &dir,
            "b.proto",
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
        let dir = scratch();
        let vendored =
            proto_fixture(&dir, "a.proto", PROTO);
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
