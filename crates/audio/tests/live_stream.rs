//! 对着真实直链跑的流式播放测试。
//!
//! 默认 `#[ignore]`:需要 bang-dream 与 axum 都在跑,还需要网易云已登录。
//!
//! ```bash
//! just bang-dream              # 终端 1
//! just server-dev              # 终端 2
//! cargo test -p audio -- --ignored
//! ```
//!
//! 单元测试喂的是 `Cursor`,证明的是解码逻辑;这一条喂的是真的 HTTP 流,
//! 证明的是**边下边读**这件事本身能走通 —— range 请求、临时文件落盘、
//! 解码器回读帧头,任何一环不成都会在这里暴露。
//!
//! 这里也是唯一能抓到「阻塞读与下载任务互等」那类死锁的地方:症状是永久挂起、
//! 没有任何报错,单元测试(数据全在内存里,永远读得到)碰不到它。
//!
//! 不测"出声":那需要真实声卡,断言不了。

use rodio::Source as _;

/// axum 后端地址,与 `crates/api` 的默认值一致。
const API_BASE: &str = "http://127.0.0.1:3000";

/// 搜一首必定存在的歌,取它的播放地址。
async fn play_url() -> String {
    let search: serde_json::Value = reqwest::get(format!(
        "{API_BASE}/search?q=%E7%B4%85%E8%93%AE%E8%8F%AF&limit=1"
    ))
    .await
    .expect("搜索请求失败(server-dev 在跑吗?)")
    .json()
    .await
    .expect("搜索响应不是 JSON");

    let id = search["tracks"][0]["id"]
        .as_str()
        .expect("搜不到任何结果");

    let source: serde_json::Value =
        reqwest::get(format!("{API_BASE}/play/{id}"))
            .await
            .expect("取播放地址失败")
            .json()
            .await
            .expect("播放地址响应不是 JSON");

    source["url"]
        .as_str()
        .expect("响应里没有 url")
        .to_owned()
}

/// 真实直链能被开流并解码出采样。
///
/// 采样率必须是个合理值 —— 拿到 HTML 错误页时解码会失败,拿到损坏音频时
/// 采样率可能是 0,两种都不该悄悄放过。
///
/// 只取一小段采样:整曲解完就退化成"整曲下载",证明不了边下边播。
#[tokio::test]
#[ignore = "需要 bang-dream + server-dev 都在跑,且网易云已登录"]
async fn loads_and_decodes_real_stream() {
    let url = play_url().await;

    let decoder =
        audio::load(&url).await.expect("开流或解码失败");

    assert!(
        decoder.sample_rate().get() >= 8_000,
        "采样率不像真的: {}",
        decoder.sample_rate()
    );

    let produced = decoder.take(4_410).count();
    assert!(produced > 0, "解码器一个采样都没产出");
}

/// 地址不是音频时报解码错误,而不是挂住或 panic。
///
/// 直链过期后网易云返回的正是一个 HTML 页面,这条走的是同一条路径。
#[tokio::test]
#[ignore = "需要 server-dev 在跑"]
async fn non_audio_url_fails_instead_of_hanging() {
    // `/health` 必定返回 JSON,不是音频。
    let error = audio::load(&format!("{API_BASE}/health"))
        .await
        .err()
        .expect("JSON 不该被当成音频");

    assert!(
        matches!(error, audio::AudioError::Decode(_)),
        "应报解码错误,实得 {error:?}"
    );
}
