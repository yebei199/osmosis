//! `.slint` 里不许写字面圆角、字号、时长和字体族名(MEO-1877)。
//!
//! 这些值收在 `theme.slint` 的 `Radius`、`TextSize`、`Motion`、`Type` 里;页面上再写一个
//! `13px`,档位就又开始散。扫的是源码文本,不是渲染结果:只认「属性名: 值」里的
//! 数字,`0px`、`0ms` 与 `self.width / 2` 这类算式不算。
//!
//! 真有理由不走档位的,写进 `EXEMPT`,一条一个理由;豁免对不上任何一行时也报红,
//! 免得改掉那行之后豁免还挂着。

use std::path::{Path, PathBuf};

/// (文件, 该行必须包含的片段, 理由)
const EXEMPT: &[(&str, &str, &str)] = &[];

#[derive(Debug, PartialEq)]
struct Finding {
    line: usize,
    kind: &'static str,
    text: String,
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// `value` 里有没有一个非零的 `<数字><unit>`。
fn has_nonzero(value: &str, unit: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit()
            || (i > 0 && is_ident(bytes[i - 1]))
        {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_digit()
                || bytes[i] == b'.')
        {
            i += 1;
        }
        let rest = &value[i..];
        let after =
            rest.as_bytes().get(unit.len()).copied();
        if rest.starts_with(unit)
            && !after.is_some_and(is_ident)
        {
            let n: f64 =
                value[start..i].parse().unwrap_or(0.0);
            if n != 0.0 {
                return true;
            }
        }
    }
    false
}

/// 取 `prop:` 之后到 `;` 的值;`prop` 两侧必须是词边界。
fn value_of<'a>(
    code: &'a str,
    prop: &str,
) -> Option<&'a str> {
    let mut from = 0;
    while let Some(pos) = code[from..].find(prop) {
        let at = from + pos;
        let before = code[..at].bytes().last();
        let after = code[at + prop.len()..].trim_start();
        if !before.is_some_and(is_ident)
            && after.starts_with(':')
        {
            return Some(
                after[1..].split(';').next().unwrap_or(""),
            );
        }
        from = at + prop.len();
    }
    None
}

fn scan(src: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, raw) in src.lines().enumerate() {
        let code = raw.split("//").next().unwrap_or("");
        let checks: [(&'static str, bool); 4] = [
            (
                "圆角",
                value_of(code, "border-radius")
                    .is_some_and(|v| has_nonzero(v, "px")),
            ),
            (
                "字号",
                value_of(code, "font-size")
                    .is_some_and(|v| has_nonzero(v, "px")),
            ),
            (
                "时长",
                value_of(code, "duration")
                    .is_some_and(|v| has_nonzero(v, "ms")),
            ),
            (
                "字体族",
                value_of(code, "font-family").is_some_and(
                    |v| v.replace("\"\"", "").contains('"'),
                ),
            ),
        ];
        for (kind, hit) in checks {
            if hit {
                out.push(Finding {
                    line: i + 1,
                    kind,
                    text: raw.trim().to_string(),
                });
            }
        }
    }
    out
}

fn slint_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in
        std::fs::read_dir(dir).expect("读 slint 目录")
    {
        let path = entry.expect("目录项").path();
        if path.is_dir() {
            slint_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e == "slint")
        {
            out.push(path);
        }
    }
}

#[test]
fn slint_sources_use_tokens_not_literals() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("slint");
    let mut files = Vec::new();
    slint_files(&dir, &mut files);
    files.sort();

    let mut failures = Vec::new();
    let mut used = vec![false; EXEMPT.len()];
    for path in &files {
        let name = path
            .strip_prefix(&dir)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        // 档位本身就定义在这里。
        if name == "theme.slint" {
            continue;
        }
        let src = std::fs::read_to_string(path).unwrap();
        for f in scan(&src) {
            let exempt = EXEMPT.iter().position(
                |(file, needle, _)| {
                    *file == name && f.text.contains(needle)
                },
            );
            match exempt {
                Some(k) => used[k] = true,
                None => failures.push(format!(
                    "{name}:{} {}:{}",
                    f.line, f.kind, f.text
                )),
            }
        }
    }
    for (k, (file, needle, _)) in EXEMPT.iter().enumerate()
    {
        if !used[k] {
            failures.push(format!(
                "豁免已失效,删掉它:{file} 「{needle}」"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "这些地方写了字面值,改用 theme.slint 里的 Radius / TextSize / Motion / Type:\n{}",
        failures.join("\n")
    );
}

#[test]
fn guard_flags_planted_literals() {
    let planted = "\
        border-radius: 10px;\n\
        font-size: Shell.compact ? 13px : TextSize.body;\n\
        animate x { duration: 200ms; easing: Motion.ease; }\n\
        font-family: \"Maple Mono NF CN\";\n";
    let kinds: Vec<_> =
        scan(planted).into_iter().map(|f| f.kind).collect();
    assert_eq!(kinds, ["圆角", "字号", "时长", "字体族"]);
}

#[test]
fn guard_accepts_tokens_and_zero() {
    let clean = "\
        border-radius: Radius.md;\n\
        border-radius: self.width / 2;\n\
        font-size: TextSize.body;\n\
        animate x { duration: root.open ? 0ms : Motion.fast; }\n\
        font-family: row.loading ? Type.body : Type.mono;\n\
        font-family: \"\";\n\
        // border-radius: 10px; 注释里的不算\n\
        interval: 450ms;\n\
        font-size-px: 3px;\n";
    assert_eq!(scan(clean), []);
}
