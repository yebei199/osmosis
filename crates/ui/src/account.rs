//! 登录页与 Rust 之间的绑定:注册、登录,以及失败时该说哪句话。
//!
//! 「说哪句话」单独成函数是因为它是**纯的**:给一个错误,得到一句人话。
//! 界面里那句话对不对,不必起窗口就能验 —— 而错的文案会把人引向错误的修复
//! (连不上时说"密码不对",人就去改一个没错的密码)。

use slint::ComponentHandle;

use crate::MainWindow;

/// 登录失败时给用户看的话。
///
/// 按服务端给的 `code` 分支,不按 HTTP 状态码 —— 契约就是这么规定的
/// (见 `contract::ErrorDto`)。没见过的 code 也要有话说:空文案等于没提示,
/// 用户只会看到按钮闪了一下。
pub fn login_failure_text(err: &api::ApiError) -> String {
    match err {
        api::ApiError::Server { code, message } => {
            match code.as_str() {
                "bad_credentials" => {
                    "用户名或密码不对".to_owned()
                }
                // 与密码错分开说 —— 都说"登录失败"的话,人会一直改密码
                "bad_invite" => "邀请码不对".to_owned(),
                "username_taken" => {
                    "这个用户名已经有人用了".to_owned()
                }
                "invalid_argument" => message.clone(),
                // 没见过的 code:把服务端那句话原样转出去,总好过沉默
                _ => message.clone(),
            }
        }
        api::ApiError::VersionMismatch { .. } => {
            "客户端与服务端版本不一致,需要更新".to_owned()
        }
        // 话没传到。**不能**说成密码问题
        api::ApiError::Transport(_) => {
            "连不上服务端,检查网络后再试".to_owned()
        }
        api::ApiError::Decode(_) => {
            "服务端的答复看不懂,可能版本不一致".to_owned()
        }
    }
}

/// 把登录页的两个回调接到 api 上。
pub fn bind(ui: &MainWindow) {
    // 启动时若已有落盘的会话,直接进主界面。它可能已被吊销 —— 那要等第一次
    // 请求失败才知道,届时由 `handle_session_expiry` 把人送回登录页。
    ui.set_logged_in(api::session::token().is_some());

    let weak = ui.as_weak();
    ui.on_login(move |username, password| {
        let weak = weak.clone();
        let (username, password) =
            (username.to_string(), password.to_string());

        spawn(weak, async move {
            api::login(&username, &password)
                .await
                .map(|_| ())
        });
    });

    let weak = ui.as_weak();
    ui.on_register(move |username, password, invite| {
        let weak = weak.clone();
        let (username, password, invite) = (
            username.to_string(),
            password.to_string(),
            invite.to_string(),
        );

        spawn(weak, async move {
            api::register(&username, &password, &invite)
                .await
                .map(|_| ())
        });
    });
}

/// 跑一次登录/注册,并把结果落到界面上。
///
/// 两条路的差别只在那个 future,收尾完全一样 —— 各写一遍的话,
/// 「记得把 busy 关掉」这件事就有两个地方会忘。
fn spawn<F>(weak: slint::Weak<MainWindow>, request: F)
where
    F: Future<Output = Result<(), api::ApiError>> + 'static,
{
    if let Some(ui) = weak.upgrade() {
        ui.set_login_busy(true);
        ui.set_login_error(slint::SharedString::new());
    }

    let _ = slint::spawn_local(async move {
        let result = request.await;

        if let Some(ui) = weak.upgrade() {
            ui.set_login_busy(false);
            match result {
                Ok(()) => ui.set_logged_in(true),
                Err(err) => ui.set_login_error(
                    login_failure_text(&err).into(),
                ),
            }
        }
    });
}

/// 这次失败是不是「登录态没了」。
///
/// 按服务端的 code 判,不按状态码:同一个 401 也可能是别的意思。
const SESSION_EXPIRED: &str = "unauthorized";

/// 会话失效时把人送回登录页,并回答"是不是这种失败"。
///
/// 任何一条路由拿到这个 code 都该走这里:token 是长期保存的,而服务端随时
/// 可能吊销 —— 不送回去的话,用户对着一个什么都拉不出来的界面,
/// 不知道自己已经掉线了。
///
/// 返回是否确实是会话失效,调用方据此决定还要不要再报一遍错 ——
/// 已经被送回登录页的人,不需要同时看到一句"失败:…"。
pub fn handle_session_expiry(
    ui: &MainWindow,
    err: &api::ApiError,
) -> bool {
    let expired = matches!(
        err,
        api::ApiError::Server { code, .. }
            if code == SESSION_EXPIRED
    );

    if expired {
        // 留下一行:清掉落盘的会话是**不可逆**的,而它此前一声不吭 ——
        // 「一重启就要重登」这类报告因此无从查起,只知道文件没了,不知道谁删的。
        // 这一行说出是哪一次请求的答复触发的。
        log::warn!(
            "会话被服务端判为失效,已清除本地登录态: {err}"
        );

        api::session::clear();
        ui.set_logged_in(false);
        ui.set_login_error("登录已失效,请重新登录".into());
    }

    expired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(code: &str) -> api::ApiError {
        api::ApiError::Server {
            code: code.to_owned(),
            message: "服务端那句话".to_owned(),
        }
    }

    /// 密码错说的是账号密码,不是网络。
    #[test]
    fn bad_credentials_reads_as_a_password_problem() {
        let text =
            login_failure_text(&server("bad_credentials"));

        assert!(
            text.contains("密码"),
            "应指向账号密码,实际 {text}"
        );
    }

    /// 邀请码错与密码错分开说 —— 都说"登录失败"的话,人会一直改密码。
    #[test]
    fn bad_invite_reads_as_an_invite_problem() {
        let text =
            login_failure_text(&server("bad_invite"));

        assert!(
            text.contains("邀请码"),
            "应指向邀请码,实际 {text}"
        );
        assert_ne!(
            text,
            login_failure_text(&server("bad_credentials")),
            "两种失败必须说不同的话"
        );
    }

    /// 连不上时不能说"用户名或密码不对" —— 那会让人去改一个没错的密码。
    #[test]
    fn network_failure_does_not_blame_the_password() {
        let text =
            login_failure_text(&api::ApiError::Transport(
                "connection refused".to_owned(),
            ));

        assert!(
            !text.contains("密码"),
            "网络问题不该赖到密码上,实际 {text}"
        );
        assert!(text.contains("网络"), "实际 {text}");
    }

    /// 没见过的 code 也要有话说。空文案等于没提示,
    /// 用户只会看到按钮闪了一下。
    #[test]
    fn an_unknown_code_still_says_something() {
        assert!(
            !login_failure_text(&server("brand_new_code"))
                .is_empty()
        );
        assert!(
            !login_failure_text(&api::ApiError::Decode(
                "x".to_owned()
            ))
            .is_empty()
        );
    }

    /// 把会话落盘处指到临时文件上。
    ///
    /// **少了这一步,跑一次测试就把开发机上真实的登录态删掉** —— `handle_session_expiry`
    /// 里那句 `session::clear()` 删的是 `~/.local/state/slint-study/session`,而它
    /// 一声不吭。症状是「每次跑完测试再开应用就要重新登录」,而人会去查应用,
    /// 查不到任何线索。`api` 那侧的会话测试早就这么防着了,这边漏了。
    fn redirect_session_to_a_temp_file() {
        let dir = std::env::temp_dir()
            .join("slint-study-ui-session");
        let _ = std::fs::create_dir_all(&dir);
        // SAFETY: 本 crate 只有这一条测试碰会话,不会与别的线程抢这个变量
        unsafe {
            std::env::set_var(
                "SLINT_STUDY_SESSION_FILE",
                dir.join("session"),
            );
        }
    }

    /// 会话失效会把人送回登录页,并让调用方知道不必再报一遍错。
    #[test]
    fn an_expired_session_sends_the_user_back() {
        redirect_session_to_a_temp_file();
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.set_logged_in(true);

        let handled = handle_session_expiry(
            &ui,
            &server(SESSION_EXPIRED),
        );

        assert!(handled, "这就是会话失效,该被认出来");
        assert!(!ui.get_logged_in(), "该回到登录页");
        assert!(
            !ui.get_login_error().is_empty(),
            "得说一句为什么被登出了,否则人只看到界面莫名跳回去"
        );
    }

    /// 别的失败不动登录态 —— 一次网络抖动把人踢下线是更糟的体验。
    #[test]
    fn other_failures_do_not_log_the_user_out() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.set_logged_in(true);

        let handled = handle_session_expiry(
            &ui,
            &api::ApiError::Transport(
                "timed out".to_owned(),
            ),
        );

        assert!(!handled);
        assert!(
            ui.get_logged_in(),
            "网络抖动不该把人踢下线"
        );
    }
}
