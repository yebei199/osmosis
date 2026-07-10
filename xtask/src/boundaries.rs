//! `cargo xtask boundaries` —— 把架构决策变成可执行的断言。
//!
//! ADR 里写的约束靠记忆是守不住的。这些检查在 CI 与本地(`just ci`)跑的是
//! **同一份代码**,不是两份互相漂移的 shell 片段。

use crate::shell::{capture, run};

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
        return Err("用法: cargo xtask boundaries".to_owned());
    }

    let checks: [(&str, Check); 3] = [
        ("contract 只依赖 serde", contract_has_no_io_crates),
        ("api 在 wasm 上不依赖 tokio", api_is_tokio_free_on_wasm),
        ("app-core 能编到 wasm", app_core_compiles_for_wasm),
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
    Err(format!("架构边界被破坏:\n  {}", failures.join("\n  ")))
}

/// ADR-0001:契约只共享线上格式,不共享 IO。
fn contract_has_no_io_crates() -> Result<(), String> {
    let tree =
        capture("cargo", &["tree", "-p", "contract", "--edges", "normal"])?;

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
    Err("api 在 wasm 上依赖了 tokio,违反 docs/adr/0002".to_owned())
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
    fn depends_on_认出直接依赖() {
        assert!(depends_on(TREE, "reqwest"));
        assert!(depends_on(TREE, "serde"));
    }

    /// 边界:名字是另一个 crate 的前缀时不能误报。
    /// `tokio-util` 不是 `tokio`,`hyper-util` 不是 `hyper`。
    #[test]
    fn depends_on_不把前缀当作匹配() {
        assert!(!depends_on(TREE, "tokio"));
        assert!(!depends_on(TREE, "hyper"));
    }

    /// 边界:空树、不存在的名字。
    #[test]
    fn depends_on_处理空输入() {
        assert!(!depends_on("", "tokio"));
        assert!(!depends_on(TREE, "sqlx"));
    }
}
