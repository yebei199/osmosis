//! 轮询该怎么走。请求本身没法在这里测(服务端地址烘在编译期),
//! 能测也值得测的是那条规则:什么时候换码、什么时候接着等。

use std::cell::Cell;

use contract::{
    QR_CONFIRMED, QR_EXPIRED, QR_SCANNED, QR_WAITING,
};
use similar_asserts::assert_eq;

use super::{QrStep, step_from};
use crate::{ApiError, QrLoginDto};

/// 换来的那张新码。
fn fresh() -> QrLoginDto {
    QrLoginDto {
        key: "new-key".to_owned(),
        url: "https://music.163.com/login?codekey=new-key"
            .to_owned(),
    }
}

/// 过期就地换一张新码回来。
///
/// 不换的话界面会一直对着一张死码轮询:用户扫它,网易云说"二维码已失效",
/// 而界面这边什么都没变 —— 看起来像扫码功能坏了。
#[tokio::test]
async fn an_expired_code_is_replaced_on_the_spot() {
    let renewals = Cell::new(0);

    let step = step_from(QR_EXPIRED, || async {
        renewals.set(renewals.get() + 1);
        Ok(fresh())
    })
    .await
    .expect("换码成功时不该是一次失败");

    assert_eq!(step, QrStep::Renewed(fresh()));
    assert_eq!(renewals.get(), 1, "该换且只换一张");
}

/// 还没到终态就不换码。
///
/// 换掉的话用户正在扫的那一张会在手里失效,而他什么都没做错。
#[tokio::test]
async fn a_live_code_is_never_replaced() {
    for (state, expected) in [
        (QR_WAITING, QrStep::Waiting),
        (QR_SCANNED, QrStep::Scanned),
        (QR_CONFIRMED, QrStep::Bound),
        // 没见过的状态按「还在等」办,而不是按「废了」办
        ("brand_new", QrStep::Waiting),
    ] {
        let renewals = Cell::new(0);

        let step = step_from(state, || async {
            renewals.set(renewals.get() + 1);
            Ok(fresh())
        })
        .await
        .expect("这一步不该失败");

        assert_eq!(step, expected, "{state} 走错了分支");
        assert_eq!(
            renewals.get(),
            0,
            "{state} 不该换码 —— 用户可能正对着这一张扫"
        );
    }
}

/// 换码失败要如实报上去,不能悄悄退回「还在等」。
///
/// 退回去的现象是界面永远停在一张过期的码上,而它看起来还好好的。
#[tokio::test]
async fn a_failed_renewal_is_reported() {
    let failed = step_from(QR_EXPIRED, || async {
        Err(ApiError::Transport(
            "connection refused".into(),
        ))
    })
    .await;

    assert!(
        failed.is_err(),
        "换码失败却报了成功,界面会停在一张死码上"
    );
}
