//! 四条网易云绑定路由各走一遍。上游由 `routes::testing` 的假 gRPC 服务扮演。
//!
//! 这一层自己没有逻辑,值得测的是**翻译**:未绑定回的是状态还是错误、
//! 状态字符串对不对得上契约、解绑有没有真的转给上游。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use contract::{
    QR_CONFIRMED, QR_EXPIRED, QR_SCANNED, QR_WAITING,
};
use similar_asserts::assert_eq;

use crate::routes::testing::{self, FakeUpstream};

use super::{create_qr, qr_state, status, unbind};

use server::bangdream::proto::{
    CreateQrLoginResponse, QrLoginState,
};

/// 摆一个假上游,并造一个干净账号。
async fn fixture(
    case: &str,
    fake: FakeUpstream,
) -> (crate::AppState, server::store::account::Account) {
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let state =
        testing::state(pool, testing::serve(fake).await);

    (state, account)
}

/// 绑好了就报昵称。
#[tokio::test]
async fn status_reports_the_bound_nickname() {
    let (state, account) = fixture(
        "ne_bound",
        FakeUpstream::logged_in_with("42", vec![]),
    )
    .await;

    let body = status(State(state), account)
        .await
        .expect("绑好的账号问状态不该失败");

    assert!(body.bound);
    assert_eq!(body.nickname.as_deref(), Some("测试账号"));
}

/// 没绑是**状态不是错误**:200 + `bound: false`。
///
/// 回 4xx 的话客户端要在错误分支里读一个正常值,而「没绑」正是个人页要显示
/// 的那个值 —— 那条路上还得先分辨它与真正的失败,而两者会长得一模一样。
#[tokio::test]
async fn an_unbound_account_is_a_state_not_a_failure() {
    let (state, account) =
        fixture("ne_unbound", FakeUpstream::default())
            .await;

    let body = status(State(state), account)
        .await
        .expect("没绑不该被当成一次失败");

    assert!(!body.bound);
    assert_eq!(
        body.nickname, None,
        "没绑就没有昵称,空串会被界面当成一个真实存在的空名字"
    );
}

/// 二维码原样搬运:key 与 url 都来自上游,这一层不渲染也不改写。
#[tokio::test]
async fn create_qr_passes_the_upstream_session_through() {
    let fake = FakeUpstream {
        qr: CreateQrLoginResponse {
            key: "the-key".to_owned(),
            url:
                "https://music.163.com/login?codekey=the-key"
                    .to_owned(),
        },
        ..FakeUpstream::default()
    };

    let (state, account) = fixture("ne_qr", fake).await;

    let body = create_qr(State(state), account)
        .await
        .expect("要一张码不该失败");

    assert_eq!(body.key, "the-key");
    assert_eq!(
        body.url,
        "https://music.163.com/login?codekey=the-key"
    );
}

/// 轮询一次:上游此刻推什么,就答什么。
///
/// 逐个状态过一遍而不是只测一个 —— 这里错一个,界面会在那一步卡住,
/// 而四种状态的界面表现完全不同(等、已扫、绑好了、换码)。
#[tokio::test]
async fn qr_state_translates_every_upstream_state() {
    for (upstream, expected) in [
        (QrLoginState::Waiting, QR_WAITING),
        (QrLoginState::Scanned, QR_SCANNED),
        (QrLoginState::Confirmed, QR_CONFIRMED),
        (QrLoginState::Expired, QR_EXPIRED),
    ] {
        let fake = FakeUpstream {
            qr_state: upstream as i32,
            ..FakeUpstream::default()
        };

        let (state, account) =
            fixture("ne_watch", fake).await;

        let body = qr_state(
            State(state),
            account,
            Path("the-key".to_owned()),
        )
        .await
        .expect("问一次进展不该失败");

        assert_eq!(
            body.state, expected,
            "上游的 {upstream:?} 该翻成 {expected}"
        );
    }
}

/// 认不出的状态当成「还在等」,让客户端继续轮询。
///
/// 判成过期的话,一张还没被扫的码会被界面换掉 —— 而用户正对着它扫。
#[tokio::test]
async fn an_unknown_upstream_state_reads_as_waiting() {
    let fake = FakeUpstream {
        qr_state: 99,
        ..FakeUpstream::default()
    };

    let (state, account) =
        fixture("ne_watch_unknown", fake).await;

    let body = qr_state(
        State(state),
        account,
        Path("the-key".to_owned()),
    )
    .await
    .expect("没见过的状态不该让这条路由失败");

    assert_eq!(body.state, QR_WAITING);
}

/// 解绑回 204,并且**真的**转给了上游。
///
/// 只断言状态码的话,一个什么都不做的 handler 也能过 —— 而那时界面会显示
/// 「已解绑」,下次点播却照样出声。
#[tokio::test]
async fn unbind_forwards_the_logout_upstream() {
    let fake = FakeUpstream::default();
    let seen = fake.clone();

    let (state, account) = fixture("ne_unbind", fake).await;

    let code = unbind(State(state), account)
        .await
        .expect("解绑不该失败");

    assert_eq!(code, StatusCode::NO_CONTENT);
    assert_eq!(
        seen.logouts(),
        1,
        "解绑必须转给上游,否则凭据还在,下次点播照样出声"
    );
}
