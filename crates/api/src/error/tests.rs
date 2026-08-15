use super::*;

/// 钉住会显示给用户的那句文案。措辞一改这里就红 —— 这是特性:
/// 文案里的每个汉字都必须在 `crates/ui/fonts/cjk-subset.ttf` 里,
/// 改了措辞就得重跑 `just font-subset`,否则 web 端显示成豆腐块。
#[test]
fn mismatch_message_contains_both_versions() {
    let err = ApiError::VersionMismatch {
        expected: 1,
        actual: 2,
    };
    assert_eq!(
        err.to_string(),
        "协议版本不匹配: 本机 v1,服务端 v2"
    );
}

/// 服务端回的 code 保留进错误里,不被压成一句文本 ——
/// 契约里那些 code 存在的全部意义就是给客户端分支用的。
#[test]
fn server_error_body_keeps_its_code() {
    let err = server_error(
        401,
        r#"{"code":"bad_credentials","message":"用户名或密码不对"}"#,
    );

    assert!(matches!(
        &err,
        ApiError::Server { code, message }
            if code == "bad_credentials"
                && message == "用户名或密码不对"
    ));
}

/// 解不出 ErrorDto 时退回 Transport,不编一个 code 出来。
/// 编了会让上层按错误的分支走,而那种错比"不知道为什么失败"更难查。
#[test]
fn unparseable_error_body_falls_back_to_transport() {
    let err = server_error(
        502,
        "<html><body>Bad Gateway</body></html>",
    );

    assert!(
        matches!(err, ApiError::Transport(_)),
        "实际 {err:?}"
    );
}
