//! `GET /app/android/{ver}.apk` —— 把 GitHub Release 上的安卓安装包转给客户端(#129)。
//!
//! 国内直连 GitHub 的资产下载只有几十 KB/s,110 MB 的包在手机上下不完;集群到
//! GitHub 快,手机到本服务也快,所以字节从这里过一道。版本号与 sha256 仍由客户端
//! 直接问 GitHub —— 这里被换了包,客户端也核不过去。
//!
//! 只转发一种地址:`{base}/v{ver}/osmosis-android-arm64-{ver}.apk`,`ver` 必须是
//! `数字.数字.数字`,别的一律 400。不让调用方决定拉哪里,本服务就当不成跳板。
//! 要登录:升级按钮只在登录后出现,不登录的人没理由来拉一百多 MB。
//!
// ponytail: 不落盘缓存,每次都回源。设备就两三台;哪天回源成了瓶颈再加。

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use contract::ErrorDto;

use server::error::{AppError, Failure, map_error};
use server::store::account::Account;

use crate::AppState;

/// 默认的回源地址。测试与自建镜像用环境变量 `APK_RELEASES_BASE` 覆盖。
pub(crate) const DEFAULT_RELEASES_BASE: &str =
    "https://github.com/yebei199/osmosis/releases/download";

/// 与 GitHub 握手最多等多久。只卡建连,理由同 `play::download` 的同名常量。
const CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// `GET /app/android/{file}`,`file` 形如 `0.1.16.apk`。
///
/// 失败只在第一个字节之前说得出口,之后断流就是截断的响应 —— 客户端按
/// GitHub 上的 sha256 核对,截断的包装不上。
pub(crate) async fn android_apk(
    State(state): State<AppState>,
    _account: Account,
    Path(file): Path<String>,
) -> Result<Response, Failure> {
    let version = version_of(&file).ok_or_else(|| {
        map_error(&AppError::Invalid("版本号应形如 0.1.16"))
    })?;
    let url = format!(
        "{}/v{version}/osmosis-android-arm64-{version}.apk",
        state.apk_releases
    );

    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|err| {
            unavailable(
                StatusCode::BAD_GATEWAY,
                &err.to_string(),
            )
        })?;
    let upstream =
        client.get(&url).send().await.map_err(|err| {
            unavailable(
                StatusCode::BAD_GATEWAY,
                &err.to_string(),
            )
        })?;
    // 状态码原样带回:404 是「这一版没有安装包」,客户端据此说清,不是笼统的网关错。
    if !upstream.status().is_success() {
        let status = StatusCode::from_u16(
            upstream.status().as_u16(),
        )
        .unwrap_or(StatusCode::BAD_GATEWAY);
        return Err(unavailable(
            status,
            &format!("上游返回 {}", upstream.status()),
        ));
    }

    let length = upstream.content_length();
    let mut response = Response::new(Body::from_stream(
        upstream.bytes_stream(),
    ));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(
            "application/vnd.android.package-archive",
        ),
    );
    if let Some(length) = length {
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from(length),
        );
    }
    Ok(response)
}

fn unavailable(
    status: StatusCode,
    detail: &str,
) -> Failure {
    tracing::warn!(detail, "安装包回源失败");
    (
        status,
        Json(ErrorDto {
            code: "upstream_failed".to_owned(),
            message: "这一版的安装包取不到".to_owned(),
        }),
    )
}

/// 从 `0.1.16.apk` 里取出 `0.1.16`。不是三段纯 ASCII 数字就是 `None`。
fn version_of(file: &str) -> Option<&str> {
    let version = file.strip_suffix(".apk")?;
    let parts: Vec<&str> = version.split('.').collect();
    let numeric = |part: &&str| {
        !part.is_empty()
            && part.len() <= 6
            && part.bytes().all(|b| b.is_ascii_digit())
    };
    (parts.len() == 3 && parts.iter().all(numeric))
        .then_some(version)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use similar_asserts::assert_eq;

    use super::*;
    use crate::routes::testing;

    #[test]
    fn only_three_numeric_parts_are_a_version() {
        assert_eq!(
            version_of("0.1.16.apk"),
            Some("0.1.16")
        );
        assert_eq!(
            version_of("10.20.300.apk"),
            Some("10.20.300")
        );
        for bad in [
            "0.1.16",
            "0.1.apk",
            "0.1.16.1.apk",
            "0.1.16-rc.1.apk",
            "v0.1.16.apk",
            "0..16.apk",
            "0.1.16.apk.apk",
            "..%2f..%2fetc.apk",
            "0.1.１6.apk",
            ".apk",
        ] {
            assert_eq!(
                version_of(bad),
                None,
                "{bad} 不该算版本号"
            );
        }
    }

    /// 在随机端口上扮 GitHub:只认 `v0.1.16` 那一个资产,别的 404。
    async fn fake_releases(body: Vec<u8>) -> String {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("绑不上回环端口");
        let addr =
            listener.local_addr().expect("取不到端口");
        let app = axum::Router::new().route(
            "/v0.1.16/osmosis-android-arm64-0.1.16.apk",
            axum::routing::get(move || {
                let body = body.clone();
                async move { body }
            }),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    async fn state(
        case: &str,
        base: String,
    ) -> (AppState, Account) {
        let pool = testing::pool().await;
        let account =
            testing::fresh_account(&pool, case).await;
        let state = AppState {
            apk_releases: base,
            ..testing::state(
                pool,
                testing::unreachable_upstream(),
            )
        };
        (state, account)
    }

    async fn get(
        state: AppState,
        account: Account,
        file: &str,
    ) -> Result<Response, Failure> {
        android_apk(
            State(state),
            account,
            Path(file.to_owned()),
        )
        .await
    }

    #[tokio::test]
    async fn the_apk_is_streamed_with_its_length() {
        let apk = b"PK\x03\x04 not really an apk".to_vec();
        let (state, account) = state(
            "apk-ok",
            fake_releases(apk.clone()).await,
        )
        .await;

        let response = get(state, account, "0.1.16.apk")
            .await
            .expect("该转发成功");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            apk.len().to_string()
        );
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/vnd.android.package-archive"
        );
        let body = axum::body::to_bytes(
            response.into_body(),
            1024 * 1024,
        )
        .await
        .expect("读不出响应体");
        assert_eq!(body.to_vec(), apk);
    }

    #[tokio::test]
    async fn a_malformed_version_is_rejected_before_any_fetch()
     {
        // 回源地址是个连不上的端口:若真去拉了,错的会是 502 而不是 400。
        let (state, account) = state(
            "apk-bad",
            "http://127.0.0.1:1".to_owned(),
        )
        .await;

        let Err((status, _)) =
            get(state, account, "..%2F..%2Fx.apk").await
        else {
            panic!("非法版本号不该被转发");
        };
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_missing_release_passes_the_upstream_status_through()
     {
        let (state, account) =
            state("apk-404", fake_releases(vec![1]).await)
                .await;

        let Err((status, Json(error))) =
            get(state, account, "0.1.99.apk").await
        else {
            panic!("上游 404 不该变成 200");
        };
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error.code, "upstream_failed");
    }

    /// 走真的路由表:不带 token 的请求在提取器那一步就被挡掉。
    #[tokio::test]
    async fn an_anonymous_request_is_refused() {
        let (state, _account) =
            state("apk-anon", fake_releases(vec![1]).await)
                .await;
        let app = axum::Router::new()
            .route(
                "/app/android/{file}",
                axum::routing::get(android_apk),
            )
            .with_state(state);
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("绑不上回环端口");
        let addr =
            listener.local_addr().expect("取不到端口");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let status = reqwest::get(format!(
            "http://{addr}/app/android/0.1.16.apk"
        ))
        .await
        .expect("连不上被测服务")
        .status();
        assert_eq!(status.as_u16(), 401);
    }
}
