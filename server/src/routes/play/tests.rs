//! `/download/{id}` 两条路各走一遍:mp3 原样透传,别的格式过 ffmpeg。
//!
//! 上游那条**临时直链**由本文件在随机端口上起的一个小 HTTP 服务扮演 ——
//! 假 gRPC 只负责说「源在这个地址」,字节从哪来它管不着,而这条路由的全部
//! 工作正是搬那些字节。
//!
//! 转码那条用真的 ffmpeg:替身能证明「我们调了它」,证明不了「出来的是
//! 合法的 mp3」,而后者才是这条路由存在的理由。

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use similar_asserts::assert_eq;

use crate::routes::testing::{self, FakeUpstream};

use super::download::download;

use server::bangdream::proto::PlaySource;

/// 响应体最多读多少 —— 测试里的音频是秒级的,几 MB 绰绰有余。
const BODY_LIMIT: usize = 16 * 1024 * 1024;

/// 在随机端口上摆一段固定字节,返回它的地址。
///
/// 端口交给内核挑(`:0`):几条测试并行时互不相撞,也不必猜一个空闲端口。
async fn serve_bytes(body: Vec<u8>) -> String {
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");

    let app = axum::Router::new().route(
        "/audio",
        axum::routing::get(move || {
            let body = body.clone();
            async move { body }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{addr}/audio")
}

/// 一条上游源:地址指向 [`serve_bytes`] 摆出来的那段字节。
fn source(url: String, format: &str) -> PlaySource {
    PlaySource {
        url,
        format: format.to_owned(),
        bit_rate: 320_000,
        ..PlaySource::default()
    }
}

/// 摆好假上游与账号,并把这条源交给它。
async fn fixture(
    case: &str,
    play_source: PlaySource,
    details: Vec<server::bangdream::proto::Track>,
) -> (crate::AppState, server::store::account::Account) {
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    let fake = FakeUpstream {
        play_source: Some(play_source),
        ..FakeUpstream::logged_in_with("42", details)
    };
    let state =
        testing::state(pool, testing::serve(fake).await);

    (state, account)
}

/// 把响应体整个读出来。
async fn body_of(
    response: axum::response::Response,
) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), BODY_LIMIT)
        .await
        .expect("响应体读不完")
        .to_vec()
}

/// 一秒 44.1kHz 立体声静音的 WAV。ffmpeg 从管道读得进去,而它不是 mp3 ——
/// 转码那条路因此有真东西可转。
fn silent_wav() -> Vec<u8> {
    const RATE: u32 = 44_100;
    const CHANNELS: u16 = 2;
    const BITS: u16 = 16;
    let data_len: u32 =
        RATE * u32::from(CHANNELS) * u32::from(BITS / 8);

    let mut wav =
        Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(
        &(RATE * u32::from(CHANNELS) * u32::from(BITS / 8))
            .to_le_bytes(),
    );
    wav.extend_from_slice(
        &(CHANNELS * BITS / 8).to_le_bytes(),
    );
    wav.extend_from_slice(&BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0);
    wav
}

/// 源已经是 mp3 时**一个字节都不动**。
///
/// 无谓地过一遍 ffmpeg 不只是浪费 CPU:mp3 转 mp3 是一次有损重编码,
/// 存下来的会比平台给的更差。
#[tokio::test]
async fn an_mp3_source_is_passed_through_byte_for_byte() {
    // 帧同步字开头,冒充一段真 mp3 —— 这条路不解码,只搬运。
    let bytes: Vec<u8> = (0..4096u32)
        .map(|i| if i < 2 { 0xFF } else { i as u8 })
        .collect();
    let url = serve_bytes(bytes.clone()).await;
    let (state, account) =
        fixture("dl_mp3", source(url, "mp3"), vec![]).await;

    let response = download(
        State(state),
        account,
        Path(testing::track_id("dl_mp3", 1)),
    )
    .await
    .expect("mp3 源应当直接给得出来");

    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(bytes.len().to_string().as_str()),
        "透传那条的长度是知道的,不给等于让客户端画不出进度"
    );
    assert_eq!(body_of(response).await, bytes);
}

/// 不是 mp3 的源要出来一段真的 mp3。
///
/// 断言交给 ffprobe:自己认帧同步字只能证明「头两个字节像 mp3」,
/// 而这条路由承诺的是整段都能放。
#[tokio::test]
async fn a_non_mp3_source_comes_back_as_real_mp3() {
    let url = serve_bytes(silent_wav()).await;
    let (state, account) =
        fixture("dl_wav", source(url, "flac"), vec![])
            .await;

    let response = download(
        State(state),
        account,
        Path(testing::track_id("dl_wav", 1)),
    )
    .await
    .expect("转码路不该失败");

    assert!(
        response
            .headers()
            .get(header::CONTENT_LENGTH)
            .is_none(),
        "转码出多少字节事前算不出来,给一个猜的长度比不给更糟"
    );

    let mp3 = body_of(response).await;
    assert!(
        !mp3.is_empty(),
        "转码出来是空的,多半是喂料与出料没有并发、把管道堵死了"
    );
    // 码率对不上精确值:ffprobe 报的是整个文件的平均值,里面还含着帧头与
    // 容器的开销。落在 320k 附近就说明 `-b:a` 真的生效了。
    let (format, bit_rate) = probe(&mp3);
    assert_eq!(
        format, "mp3",
        "ffprobe 认不出来的东西,系统音乐库同样扫不到"
    );
    assert!(
        (310_000..=330_000).contains(&bit_rate),
        "码率应当在 320k 附近,实际 {bit_rate}"
    );
}

/// 只有试听片段的源要带自己那个 code 被拒。
///
/// 笼统的 403 不够用:客户端要据此说清「这首要会员」,而那句话与别的
/// 「不让做」长得完全不一样。
#[tokio::test]
async fn a_trial_only_source_is_refused_with_its_own_code()
{
    let url = serve_bytes(vec![0; 16]).await;
    let (state, account) = fixture(
        "dl_trial",
        PlaySource {
            trial: true,
            ..source(url, "mp3")
        },
        vec![],
    )
    .await;

    let (code, body) = download(
        State(state),
        account,
        Path(testing::track_id("dl_trial", 1)),
    )
    .await
    .expect_err("试听片段不该下得下来");

    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(body.code, contract::TRIAL_ONLY);
}

/// 文件名来自曲目详情,并且两份都要在 —— 歌名几乎一定带非 ASCII,
/// 而请求头只容得下可见 ASCII。
#[tokio::test]
async fn the_file_name_follows_the_track_detail() {
    let url =
        serve_bytes(vec![0xFF, 0xFB, 0x90, 0x00]).await;
    let mut track = testing::upstream_track(
        &testing::track_id("dl_name", 1),
        "残響散歌",
    );
    track.artists[0].name = "Aimer".to_owned();
    let (state, account) =
        fixture("dl_name", source(url, "mp3"), vec![track])
            .await;

    let response = download(
        State(state),
        account,
        Path(testing::track_id("dl_name", 1)),
    )
    .await
    .expect("mp3 源应当直接给得出来");

    let disposition = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .expect("没有 Content-Disposition,浏览器与下载器都只能用 URL 当名字")
        .to_owned();

    assert!(
        disposition.contains(
            "filename*=UTF-8''Aimer%20-%20%E6%AE%8B%E9%9F%BF%E6%95%A3%E6%AD%8C.mp3"
        ),
        "真名要按 RFC 5987 给:{disposition}"
    );
    assert!(
        disposition
            .contains("filename=\"Aimer - ____.mp3\""),
        "还要一份纯 ASCII 的兜底:{disposition}"
    );
}

/// 平台那条直链已经过期(回 403 错误页)时,不能把错误页当音频转出去。
///
/// 转出去的话,手机上会多一个几百字节、播不出声的 "mp3",
/// 而从头到尾没有任何一层报过错。
#[tokio::test]
async fn an_upstream_error_page_is_not_served_as_audio() {
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/audio",
            axum::routing::get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    "<html>expired</html>",
                )
            }),
        );
        let _ = axum::serve(listener, app).await;
    });

    let (state, account) = fixture(
        "dl_expired",
        source(format!("http://{addr}/audio"), "mp3"),
        vec![],
    )
    .await;

    let (code, _) = download(
        State(state),
        account,
        Path(testing::track_id("dl_expired", 1)),
    )
    .await
    .expect_err("平台的错误页不该被当成音频");

    assert_eq!(code, StatusCode::BAD_GATEWAY);
}

/// 问 ffprobe:这段字节是什么格式、多少码率。
fn probe(bytes: &[u8]) -> (String, i64) {
    use std::io::Write as _;

    // ffprobe 要一个能 seek 的输入,管道给不了。后缀留着给它当格式提示
    let mut file = tempfile::Builder::new()
        .suffix(".mp3")
        .tempfile()
        .expect("建不出临时文件");
    file.write_all(bytes).expect("写不进临时文件");

    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=format_name,bit_rate",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file.path())
        .output()
        .expect("跑不了 ffprobe —— 转码路要求它在 PATH 上");

    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    (
        lines.next().unwrap_or_default().to_owned(),
        lines
            .next()
            .unwrap_or_default()
            .parse()
            .unwrap_or_default(),
    )
}
