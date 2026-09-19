//! `GET /download/{track_id}` —— 把一首歌当作 mp3 文件交出去。
//!
//! 与 [`super::play`] 共用同一条上游取源(同一个档位),差别只在交付方式:
//! 那边交出一条**客户端自己去取**的临时直链,这边把字节从上游拉过来交出去。
//!
//! **格式一律归一成 mp3**。源已经是 mp3 就原样透传;flac 之类的过一遍 ffmpeg。
//! 转码放在这一侧而不是客户端:LAME 是 C 依赖,安卓要交叉编译进包、wasm 端
//! 根本编不了,而手机上转一首 flac 是几十秒的满载 CPU(见 issue #97)。
//!
//! ## 失败只在第一个字节之前说得出口
//!
//! 一旦 `200` 与响应头发出去,HTTP/1.1 就没有回头改状态码的余地了。所以这里的
//! 顺序是:取源 → 起 ffmpeg → 确认上游那条连接真的开了 → **再**返回流式 body。
//! 这之后的失败(上游断流、ffmpeg 中途死掉)只能表现为**截断的响应**,
//! 客户端必须自己判断收全了没有 —— 落盘那一侧因此要写进临时态、收全才提交。

use std::process::Stdio;

use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use contract::{ErrorDto, TRIAL_ONLY, download_file_name};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use server::account::Account;
use server::bangdream::{
    self,
    proto::{
        GetPlaySourceRequest, GetTracksRequest, Platform,
        PlaySource,
    },
};
use server::error::Failure;

use crate::routes::play::PLAY_QUALITY;
use crate::{AppState, fail};

/// 转码的目标码率。定值 —— 界面上没有任何地方能选它,做到音质选择时再提成参数。
const MP3_BITRATE: &str = "320k";

/// 与上游握手最多等多久。
///
/// 只卡**建连**,不卡整条传输:一首无损几十 MB,给整条传输设上限等于给大文件
/// 设一个隐形的体积上限,而那个上限会表现为「下到一半就断」。
const CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// `GET /download/{track_id}` —— 整首歌的 mp3 字节。
pub(crate) async fn download(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<Response, Failure> {
    let source =
        play_source(&state, &account, &track_id).await?;

    // 试听片段单独一个 code:客户端要据此说清「这首要会员」,
    // 而不是笼统地报一句"不让下"。
    if source.trial {
        return Err(trial_only());
    }

    let file_name =
        file_name_of(&state, &account, &track_id).await;
    let upstream = fetch(&source.url).await?;

    // 上游给的 format 是**它认为**的容器格式。按它分路,而不是嗅探字节:
    // 嗅探要先读一段再决定,那段字节还得接回流里去。
    if source.format.eq_ignore_ascii_case("mp3") {
        let length = upstream.content_length();
        Ok(respond(
            &file_name,
            length,
            Body::from_stream(upstream.bytes_stream()),
        ))
    } else {
        // 转码那一路没有 Content-Length:ffmpeg 出多少字节事前算不出来。
        Ok(respond(&file_name, None, transcode(upstream)?))
    }
}

/// 向上游要这一首的播放源。与 [`super::play`] 同一个档位 ——
/// 下载与播放拿到的必须是同一条源,否则「听到的」和「存下的」会是两个版本。
async fn play_source(
    state: &AppState,
    account: &Account,
    track_id: &str,
) -> Result<PlaySource, Failure> {
    let mut catalog = state.upstream.catalog.clone();
    let response = catalog
        .get_play_source(bangdream::as_user(
            account,
            GetPlaySourceRequest {
                platform: Platform::Netease as i32,
                track_id: track_id.to_owned(),
                level: PLAY_QUALITY as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    response.source.ok_or_else(|| {
        fail(&tonic::Status::internal("上游没有返回播放源"))
    })
}

/// 这首歌存下来该叫什么。
///
/// 详情取不到**不是失败**:名字退回曲目 id,歌照样下得下来。为了一个文件名
/// 让整次下载失败,是把装饰性的东西提成了必要条件。
async fn file_name_of(
    state: &AppState,
    account: &Account,
    track_id: &str,
) -> String {
    let mut catalog = state.upstream.catalog.clone();
    let track = catalog
        .get_tracks(bangdream::as_user(
            account,
            GetTracksRequest {
                platform: Platform::Netease as i32,
                track_ids: vec![track_id.to_owned()],
            },
        ))
        .await
        .ok()
        .and_then(|response| {
            response.into_inner().tracks.into_iter().next()
        })
        .map(bangdream::track_to_dto);

    match track {
        Some(track) => {
            download_file_name(&track.artists, &track.title)
        }
        // 详情取不到时退回曲目 id —— 同一个规则,只是歌手与歌名都没有。
        None => download_file_name(&[], track_id),
    }
}

/// 把上游那条直链打开,并确认它真的给了内容。
///
/// 不检查状态码的话,平台的 403 错误页会被当成音频原样转出去 —— 落到手机上是
/// 一个几百字节、播不出声的 "mp3",而没有任何一层报过错。
async fn fetch(
    url: &str,
) -> Result<reqwest::Response, Failure> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|err| {
            internal("取不到音频", &err.to_string())
        })?;

    let response =
        client.get(url).send().await.map_err(|err| {
            internal("取不到音频", &err.to_string())
        })?;

    if !response.status().is_success() {
        return Err(internal(
            "平台没给音频",
            &format!("上游返回 {}", response.status()),
        ));
    }

    Ok(response)
}

/// 起一个 ffmpeg,上游字节喂给它的 stdin,它的 stdout 就是响应体。
///
/// `kill_on_drop`:客户端半途断开时 body 被丢掉,stdout 随之关闭 —— 没有这一行,
/// 进程会挂在那里直到写满管道缓冲才死,一台机器上攒几十个是常态。
fn transcode(
    upstream: reqwest::Response,
) -> Result<Body, Failure> {
    let mut child = tokio::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-f",
            "mp3",
            "-b:a",
            MP3_BITRATE,
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            internal("转不了这个格式", &err.to_string())
        })?;

    let mut stdin =
        child.stdin.take().expect("stdin 刚设成 piped");
    let stdout =
        child.stdout.take().expect("stdout 刚设成 piped");

    // 喂料与出料必须并发:ffmpeg 的 stdout 没人读时管道会写满,它随即停下来
    // 不再读 stdin —— 两根管子互相等着,就是一次死锁。
    tokio::spawn(async move {
        let mut bytes = upstream.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let Ok(chunk) = chunk else {
                tracing::warn!(
                    "上游音频流中断,转码只能截断"
                );
                break;
            };
            if stdin.write_all(&chunk).await.is_err() {
                // ffmpeg 已经退了(多半是客户端断开连带把它杀了),不是错误。
                break;
            }
        }
        // stdin 落出作用域即关闭,ffmpeg 据此知道输入到头了并收尾写出最后一帧。
    });

    // child 必须活到转码结束,所以交给一个任务去 wait ——
    // 就地 drop 的话 `kill_on_drop` 会在第一个字节出来之前把它杀掉。
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) if !status.success() => {
                tracing::warn!(%status, "ffmpeg 非正常退出,这次下载是截断的");
            }
            Err(err) => {
                tracing::warn!(%err, "等不到 ffmpeg 退出");
            }
            _ => {}
        }
    });

    Ok(Body::from_stream(
        tokio_util::io::ReaderStream::new(stdout),
    ))
}

/// 装配响应头:文件名与(可知时的)长度。
fn respond(
    file_name: &str,
    length: Option<u64>,
    body: Body,
) -> Response {
    let mut response = Response::new(body);
    let headers = response.headers_mut();

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("audio/mpeg"),
    );
    if let Ok(value) =
        HeaderValue::from_str(&disposition(file_name))
    {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Some(length) = length {
        headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from(length),
        );
    }

    response
}

/// `Content-Disposition` 的值。
///
/// 两份文件名都要给:歌名几乎一定带非 ASCII,而请求头只容得下可见 ASCII。
/// `filename*`(RFC 5987)是真名,`filename` 是给读不懂前者的一方兜底的。
fn disposition(file_name: &str) -> String {
    let ascii: String = file_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric()
                || matches!(ch, '-' | '_' | '.' | ' ')
            {
                ch
            } else {
                '_'
            }
        })
        .collect();

    format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{}",
        percent_encode(file_name)
    )
}

/// 按 RFC 5987 转义:unreserved 之外的字节逐个转成 `%XX`。
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => out.push(*byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 只有试听片段时的 403。
///
/// 不走 `error::forbidden`:那一个的 code 写死成 `forbidden`,而客户端要分辨
/// 「这首要会员」和别的不让做的事。
fn trial_only() -> Failure {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorDto {
            code: TRIAL_ONLY.to_owned(),
            message: "这首歌只给得出试听片段,下不了整首"
                .to_owned(),
        }),
    )
}

/// 本服务这一侧没办成:细节只进日志,客户端拿它也没办法。
fn internal(message: &str, detail: &str) -> Failure {
    tracing::error!(detail, "下载失败");
    (
        StatusCode::BAD_GATEWAY,
        Json(ErrorDto {
            code: "upstream_failed".to_owned(),
            message: message.to_owned(),
        }),
    )
}
