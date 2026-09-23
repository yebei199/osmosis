//! S3 客户端对着真的 RustFS 跑:存、问在不在、签给客户端的链接能取(含 Range)、删。
//!
//! 起容器见 `just rustfs`。每条测试用自己的键,并行跑互不相干。

use server::objects::{Objects, S3, S3Config};

/// 与 justfile 的 `rustfs` 配方一致。
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:9900";

fn config(endpoint: &str) -> S3Config {
    S3Config {
        endpoint: endpoint.to_owned(),
        public_endpoint: endpoint.to_owned(),
        bucket: "osmosis-test".to_owned(),
        region: "us-east-1".to_owned(),
        access_key_id: "devonly".to_owned(),
        secret_access_key: "devonly-secret".to_owned(),
    }
}

async fn s3() -> S3 {
    let endpoint = std::env::var("S3_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned());
    let s3 = S3::new(config(&endpoint)).expect("配置应当合法");
    s3.ensure_bucket().await.unwrap_or_else(|err| {
        panic!(
            "连不上 RustFS({endpoint}): {err}\n起一个:just rustfs"
        )
    });
    s3
}

/// 一段有模式的字节,切一段出来也认得出是哪一段。
fn payload() -> Vec<u8> {
    (0..64 * 1024u32).map(|i| (i % 251) as u8).collect()
}

/// 存进去就在,签出来的链接取得回同样的字节,删了就不在。
#[tokio::test]
async fn put_then_fetch_then_delete() {
    let s3 = s3().await;
    let key = "tests/roundtrip.mp3";
    let bytes = payload();

    s3.put(key, bytes.clone(), "audio/mpeg")
        .await
        .expect("存应当成功");
    assert!(s3.exists(key).await.expect("问得到"));

    let fetched = reqwest::get(s3.presign_get(key))
        .await
        .expect("链接应当取得到");
    assert_eq!(fetched.status(), 200);
    assert_eq!(
        fetched
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("audio/mpeg")
    );
    assert_eq!(fetched.bytes().await.unwrap(), bytes);

    s3.delete(key).await.expect("删应当成功");
    assert!(!s3.exists(key).await.expect("问得到"));
}

/// 客户端边放边按 Range 取、拖动进度也靠它 —— 签出来的链接必须认 Range。
#[tokio::test]
async fn presigned_link_honours_range() {
    let s3 = s3().await;
    let key = "tests/range.flac";
    let bytes = payload();
    s3.put(key, bytes.clone(), "audio/flac")
        .await
        .expect("存应当成功");

    let partial = reqwest::Client::new()
        .get(s3.presign_get(key))
        .header(reqwest::header::RANGE, "bytes=1000-1999")
        .send()
        .await
        .expect("链接应当取得到");

    assert_eq!(partial.status(), 206);
    assert_eq!(
        partial.bytes().await.unwrap(),
        bytes[1000..2000]
    );
}

/// 从来没存过的键答「不在」而不是报错;删它也不报错。
#[tokio::test]
async fn a_missing_key_is_absent_not_an_error() {
    let s3 = s3().await;
    let key = "tests/never-put.mp3";

    assert!(!s3.exists(key).await.expect("问得到"));
    s3.delete(key).await.expect("删不存在的也算成功");
}

/// RustFS 不在时是 `Err`,不是「不在」—— 调用方据此退回网易云,
/// 而不是把表里那一行当成对象丢了删掉。
#[tokio::test]
async fn an_unreachable_store_is_an_error() {
    // 端口 1 本机不会有人听,连接立刻被拒
    let s3 = S3::new(config("http://127.0.0.1:1"))
        .expect("配置应当合法");

    assert!(s3.exists("tests/any.mp3").await.is_err());
}
