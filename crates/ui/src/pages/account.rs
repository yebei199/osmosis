//! 登录页与 Rust 之间的绑定:注册、登录,以及失败时该说哪句话。
//!
//! 「说哪句话」单独成函数是因为它是**纯的**:给一个错误,得到一句人话。
//! 界面里那句话对不对,不必起窗口就能验 —— 而错的文案会把人引向错误的修复
//! (连不上时说"密码不对",人就去改一个没错的密码)。

use slint::ComponentHandle;

use crate::Library;
use crate::MainWindow;
use crate::Session;

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

/// 服务端说「这个账号还没绑网易云」时给的 code(见 server 的 `error::map_status`)。
const NETEASE_UNBOUND: &str = "netease_not_logged_in";

/// 网易云没绑时给用户看的话。
///
/// 只写一遍:播放状态行与那条能点进个人页的通知说的是同一句 ——
/// 各写一遍的话,改了一处另一处就成了另一件事。
pub const NETEASE_UNBOUND_TEXT: &str =
    "网易云未登录,去个人页扫码绑定";

/// 这次失败是不是「网易云还没绑」。
///
/// 按 code 判而不是按 HTTP 状态码,也不按上游那句话的措辞 —— 它来自网易云,
/// 想怎么变就怎么变。
pub fn netease_unbound(err: &api::ApiError) -> bool {
    matches!(
        err,
        api::ApiError::Server { code, .. }
            if code == NETEASE_UNBOUND
    )
}

/// 一次普通请求失败时给用户看的话(取曲目、点播这一类)。
///
/// 与 [`login_failure_text`] 分开:那一份服务的是登录页,同一个 code 在两处
/// 的意思不一样。这里只改写一种 —— 上游原话是「netease: 未登录」,读的人
/// 会以为是**本应用**的登录掉了,于是去重登一个好好的账号。
pub fn request_failure_text(err: &api::ApiError) -> String {
    if netease_unbound(err) {
        return NETEASE_UNBOUND_TEXT.to_owned();
    }

    err.to_string()
}

/// 报一次请求失败。
///
/// 网易云没绑的那一种走**能点进个人页**的通知:用户此刻要去的正是那一页,
/// 而说出问题却不给去处,他只能自己在四个页签里找。其余照旧一句
/// 「什么什么失败: 原因」。
pub fn report_failure(
    ui: &MainWindow,
    what: &str,
    err: &api::ApiError,
) {
    if netease_unbound(err) {
        crate::notice::show_to_profile(
            ui,
            NETEASE_UNBOUND_TEXT.to_owned(),
        );
    } else {
        crate::notice::show(ui, format!("{what}: {err}"));
    }
}

/// 把登录页的两个回调接到 api 上。
pub fn bind(ui: &MainWindow) {
    // 启动时若已有落盘的会话,直接进主界面。它可能已被吊销 —— 那要等第一次
    // 请求失败才知道,届时由 `handle_session_expiry` 把人送回登录页。
    ui.global::<Session>()
        .set_logged_in(api::session::token().is_some());

    let weak = ui.as_weak();
    ui.global::<Session>().on_login(
        move |username, password| {
            let weak = weak.clone();
            let (username, password) = (
                username.to_string(),
                password.to_string(),
            );

            spawn(weak, async move {
                api::login(&username, &password)
                    .await
                    .map(|_| ())
            });
        },
    );

    let weak = ui.as_weak();
    ui.global::<Session>().on_register(
        move |username, password, invite| {
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
        },
    );

    // 设置页的退出登录:清掉落盘会话,回登录页。与 `handle_session_expiry`
    // 的收尾同一件事,只是这次是用户自己要走的。
    let weak = ui.as_weak();
    ui.global::<Session>().on_logout(move || {
        let Some(ui) = weak.upgrade() else { return };
        api::session::clear();
        ui.global::<Session>().set_logged_in(false);
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
        ui.global::<Session>().set_busy(true);
        ui.global::<Session>()
            .set_error(slint::SharedString::new());
    }

    let _ = slint::spawn_local(async move {
        let result = request.await;

        if let Some(ui) = weak.upgrade() {
            ui.global::<Session>().set_busy(false);
            match result {
                Ok(()) => on_login_succeeded(&ui),
                Err(err) => {
                    ui.global::<Session>().set_error(
                        login_failure_text(&err).into(),
                    )
                }
            }
        }
    });
}

/// 登录成功的收尾。
///
/// 两件事:置上登录态,以及**补拉一次红心集合**。后者不是顺手做的 ——
/// 拉那个集合的唯一一次请求跑在绑定阶段,也就是登录之前(`build_ui` 先
/// `session::restore` 再绑界面)。本次会话内才首次登录的人,那一次不带
/// token,失败后按 `liked::refresh` 的规矩静默降级成空集合,屏幕上的现象是
/// **整页所有歌**的心都是空的,看起来像红心全没了。
///
/// 恢复出来的会话不走这里:那条路上的请求本来就带得上 token。
fn on_login_succeeded(ui: &MainWindow) {
    ui.global::<Session>().set_logged_in(true);
    ui.global::<Library>().invoke_refresh_liked();
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
        to_login_page(ui, &format!("{err}"));
    }

    expired
}

/// 清掉登录态,把人送回登录页。
///
/// 两条路进来:HTTP 那侧拿到 `unauthorized`(见上),以及同播的信令被服务端
/// 判为 401。两者是同一件事 —— token 不作数了 —— 所以善后也必须是同一份,
/// 分两份写的话迟早只改了一处。
///
/// `cause` 只进日志:清掉落盘的会话是**不可逆**的,而它此前一声不吭 ——
/// 「一重启就要重登」这类报告因此无从查起,只知道文件没了,不知道谁删的。
/// 所以走 `expire` 而不是 `clear`:落盘那份先留成 `session.bak`(#127)。
pub(crate) fn to_login_page(ui: &MainWindow, cause: &str) {
    log::warn!(
        "会话被服务端判为失效,已清除本地登录态: {cause}"
    );

    api::session::expire();
    ui.global::<Session>().set_logged_in(false);
    ui.global::<Session>()
        .set_error("登录已失效,请重新登录".into());
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

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

    /// 「网易云没绑」要被认出来,且只认它。
    ///
    /// 认错的代价是把一句「去个人页扫码」贴到一个与网易云无关的失败上,
    /// 而那条路点过去什么也解决不了。
    #[test]
    fn only_the_netease_code_counts_as_unbound() {
        assert!(netease_unbound(&server(
            "netease_not_logged_in"
        )));
        assert!(!netease_unbound(&server("not_found")));
        assert!(!netease_unbound(
            &api::ApiError::Transport(
                "timed out".to_owned()
            )
        ));
    }

    /// 网易云没绑要说成「去个人页扫码」,不能原样转上游那句「netease: 未登录」。
    ///
    /// 原话会被读成**本应用**的登录掉了,于是用户去重登一个好好的账号,
    /// 而点播照样不出声。
    #[test]
    fn an_unbound_netease_points_at_the_profile_page() {
        let text = request_failure_text(&server(
            "netease_not_logged_in",
        ));

        assert!(
            text.contains("个人页"),
            "得说清去哪儿绑,实际 {text}"
        );
    }

    /// 别的失败原样转出去,不被这条改写吞掉。
    #[test]
    fn other_failures_keep_their_own_words() {
        let text = request_failure_text(
            &api::ApiError::Transport(
                "connection refused".to_owned(),
            ),
        );

        assert!(text.contains("网络"), "实际 {text}");
    }

    /// 网易云那一句要**点得动**:横幅带上个人页的位次。
    ///
    /// 只有文案没有去处的话,用户读完还得自己在四个页签里找 ——
    /// 而这条提示正是在他不知道该去哪儿的时候出现的。
    #[test]
    fn the_unbound_notice_can_be_clicked_through() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");

        report_failure(
            &ui,
            "取曲目失败",
            &server("netease_not_logged_in"),
        );

        assert_eq!(
            ui.global::<crate::Shell>().get_banner_text(),
            NETEASE_UNBOUND_TEXT
        );
        assert_eq!(
            ui.global::<crate::Shell>().get_banner_tab(),
            2,
            "这一句该能点进个人页"
        );
    }

    /// 别的失败不带去处 —— 点它跳到一个与失败无关的页面更糟。
    #[test]
    fn an_ordinary_failure_has_nowhere_to_go() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");

        report_failure(
            &ui,
            "取曲目失败",
            &api::ApiError::Transport(
                "timed out".to_owned(),
            ),
        );

        assert!(
            ui.global::<crate::Shell>()
                .get_banner_text()
                .starts_with("取曲目失败"),
            "该照旧报出是哪件事失败了"
        );
        assert_eq!(
            ui.global::<crate::Shell>().get_banner_tab(),
            -1,
            "没有去处的提示不该点得动"
        );
    }

    /// 把会话落盘处指到临时文件上。
    ///
    /// **少了这一步,跑一次测试就把开发机上真实的登录态删掉** —— `handle_session_expiry`
    /// 里那句 `session::expire()` 挪走的是 `~/.local/state/osmosis-dev/session`,而它
    /// 一声不吭。症状是「每次跑完测试再开应用就要重新登录」,而人会去查应用,
    /// 查不到任何线索。`api` 那侧的会话测试早就这么防着了,这边漏了。
    fn redirect_session_to_a_temp_file() {
        let dir =
            std::env::temp_dir().join("osmosis-ui-session");
        let _ = std::fs::create_dir_all(&dir);
        // SAFETY: 本 crate 只有这一条测试碰会话,不会与别的线程抢这个变量
        unsafe {
            std::env::set_var(
                "OSMOSIS_SESSION_FILE",
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
        ui.global::<Session>().set_logged_in(true);

        let handled = handle_session_expiry(
            &ui,
            &server(SESSION_EXPIRED),
        );

        assert!(handled, "这就是会话失效,该被认出来");
        assert!(
            !ui.global::<Session>().get_logged_in(),
            "该回到登录页"
        );
        assert!(
            !ui.global::<Session>().get_error().is_empty(),
            "得说一句为什么被登出了,否则人只看到界面莫名跳回去"
        );
    }

    /// 别的失败不动登录态 —— 一次网络抖动把人踢下线是更糟的体验。
    #[test]
    fn other_failures_do_not_log_the_user_out() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.global::<Session>().set_logged_in(true);

        let handled = handle_session_expiry(
            &ui,
            &api::ApiError::Transport(
                "timed out".to_owned(),
            ),
        );

        assert!(!handled);
        assert!(
            ui.global::<Session>().get_logged_in(),
            "网络抖动不该把人踢下线"
        );
    }

    /// 只有服务端明确说 token 无效才算失效(#127)。协议版本不对、限流、
    /// 响应体读不懂,token 都还好好的 —— 删了就得重登,而删会话不可逆。
    #[test]
    fn only_an_invalid_token_counts_as_expiry() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");
        ui.global::<Session>().set_logged_in(true);

        let not_expiry = [
            api::ApiError::VersionMismatch {
                expected: 2,
                actual: 1,
            },
            server("rate_limited"),
            server("bad_credentials"),
            api::ApiError::Decode("<html>502</html>".to_owned()),
        ];

        for err in &not_expiry {
            assert!(
                !handle_session_expiry(&ui, err),
                "{err} 不是会话失效"
            );
        }
        assert!(ui.global::<Session>().get_logged_in());
    }

    /// 一个把 `refresh-liked` 的调用次数记下来的窗口。
    fn window_counting_refreshes()
    -> (MainWindow, Rc<Cell<usize>>) {
        i_slint_backend_testing::init_no_event_loop();
        let ui = MainWindow::new().expect("建不出主窗口");

        let count = Rc::new(Cell::new(0));
        let seen = count.clone();
        ui.global::<Library>().on_refresh_liked(
            move || {
                seen.set(seen.get() + 1);
            },
        );

        (ui, count)
    }

    /// 登录成功要补拉一次红心集合。
    ///
    /// 决定每行心红还是空的那个集合,整个进程只拉一次,而那一次跑在登录
    /// **之前**(`build_ui` 先 `session::restore` 再绑界面,`music::bind`
    /// 里那次 refresh 因此不带 token)。本次会话内首次登录的用户,那一次
    /// 必然失败,而失败是静默降级成空集合的 —— 屏幕上的现象是**整页所有
    /// 歌**的心都是空的,看起来像红心全没了。
    #[test]
    fn logging_in_refreshes_the_liked_set() {
        let (ui, refreshes) = window_counting_refreshes();

        on_login_succeeded(&ui);

        assert_eq!(
            refreshes.get(),
            1,
            "登录成功该补拉一次红心集合,否则整页的心都是空的"
        );
    }

    /// 补拉不能顶掉登录本来的职责:登录态照样要置上。
    ///
    /// 两件事挤在同一个收尾里,加一件容易漏掉另一件,而漏了的现象是
    /// 登录成功却停在登录页。
    #[test]
    fn logging_in_still_marks_the_session_as_logged_in() {
        let (ui, _refreshes) = window_counting_refreshes();
        assert!(
            !ui.global::<Session>().get_logged_in(),
            "开局该是没登录"
        );

        on_login_succeeded(&ui);

        assert!(
            ui.global::<Session>().get_logged_in(),
            "登录态该置上"
        );
    }

    /// 边界:恢复出来的会话不走登录收尾,因此不该触发补拉。
    ///
    /// 那条路上的 refresh 本来就带得上 token(`build_ui` 先 `restore`
    /// 再绑),再补一次是白发一个请求。
    #[test]
    fn restoring_a_session_does_not_go_through_the_login_path()
     {
        let (ui, refreshes) = window_counting_refreshes();

        // 恢复会话走的是 bind 里那句 set_logged_in,不经过登录收尾
        ui.global::<Session>().set_logged_in(true);

        assert_eq!(
            refreshes.get(),
            0,
            "恢复会话那条路本来就带得上 token,不必再补一次"
        );
    }
}
