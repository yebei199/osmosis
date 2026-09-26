//! 存歌是边下边传的:内存里只有路上的那几块,和单曲多大无关(#147 生产 OOM)。
//!
//! 判据不量进程内存(同一进程里别的测试也在分配,量不准),而量「在路上」的字节:
//! 上游已经吐出、对象存储还没收到的那部分。整首先读进内存再上传的话,对象存储
//! 收到第一块时上游早已吐完整首,这个数就等于整首的大小。

use std::sync::Mutex;
use std::sync::atomic::AtomicU64;

use axum::body::Body;
use axum::http::header;
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use server::objects::{ObjectBody, ObjectResult, Objects};
use similar_asserts::assert_eq;

use super::*;

/// 假上游吐多少:24bit 无损单曲的量级再翻一倍多。
const TRACK: u64 = 256 * 1024 * 1024;

/// 每块多大。只是假上游的切法,与被测代码无关。
const CHUNK: usize = 64 * 1024;

/// 「在路上」的上限。本机回环的收发缓冲加上 hyper 的读缓冲是几 MB,
/// 整首缓冲则是 [`TRACK`] 那么大 —— 两者隔着一个数量级。
const IN_FLIGHT_CEILING: u64 = 32 * 1024 * 1024;

/// 在随机端口上吐 `TRACK` 字节的 FLAC:开头是 96k/24bit 的 STREAMINFO,
/// 故意切在头的中间,读头的那段要跨块拼。`emitted` 数着吐出去多少。
/// `with_length` 为假时不给 Content-Length(分块传输)。
async fn serve_big_flac(
    emitted: Arc<AtomicU64>,
    with_length: bool,
) -> String {
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上回环端口");
    let addr = listener.local_addr().expect("取不到端口");
    let app = axum::Router::new().route(
        "/audio",
        axum::routing::get(move || {
            let emitted = emitted.clone();
            async move {
                let head = flac_header(96_000, 24);
                let filler = Bytes::from(vec![0u8; CHUNK]);
                let rest = TRACK as usize - head.len();
                let mut chunks = vec![
                    Bytes::copy_from_slice(&head[..10]),
                    Bytes::copy_from_slice(&head[10..]),
                ];
                chunks.extend(
                    (0..rest / CHUNK)
                        .map(|_| filler.clone()),
                );
                chunks.push(filler.slice(..rest % CHUNK));
                let body = futures_util::stream::iter(
                    chunks,
                )
                .map(move |chunk| {
                    emitted.fetch_add(
                        chunk.len() as u64,
                        Ordering::SeqCst,
                    );
                    Ok::<_, std::io::Error>(chunk)
                });
                let mut response =
                    axum::response::Response::new(
                        Body::from_stream(body),
                    );
                if with_length {
                    response.headers_mut().insert(
                        header::CONTENT_LENGTH,
                        TRACK.into(),
                    );
                }
                response
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/audio")
}

/// 只数不存的对象存储:收到多少、在路上最多有多少(每到一块时,上游已吐出的
/// 减去这一块之前收到的)。
struct Gauge {
    emitted: Arc<AtomicU64>,
    received: AtomicU64,
    peak_in_flight: AtomicU64,
    length: Mutex<Option<u64>>,
}

impl Objects for Gauge {
    fn put(
        &self,
        _key: &str,
        mut body: ObjectBody,
        length: u64,
        _content_type: &'static str,
    ) -> BoxFuture<'_, ObjectResult<()>> {
        *self.length.lock().unwrap() = Some(length);
        Box::pin(async move {
            while let Some(chunk) = body.next().await {
                let chunk =
                    chunk.map_err(|err| err.to_string())?;
                // 手里这一块也算在路上:整首缓冲时它就是整首
                let before = self.received.fetch_add(
                    chunk.len() as u64,
                    Ordering::SeqCst,
                );
                let in_flight = self
                    .emitted
                    .load(Ordering::SeqCst)
                    .saturating_sub(before);
                self.peak_in_flight
                    .fetch_max(in_flight, Ordering::SeqCst);
            }
            Ok(())
        })
    }

    fn exists(
        &self,
        _key: &str,
    ) -> BoxFuture<'_, ObjectResult<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn delete(
        &self,
        _key: &str,
    ) -> BoxFuture<'_, ObjectResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn presign_get(&self, key: &str) -> String {
        format!("gauge://{key}")
    }
}

/// 假上游吐大文件、对象存储换成 [`Gauge`] 的一套环境。
async fn big_fixture(
    case: &str,
    with_length: bool,
) -> (AppState, Account, Arc<Gauge>) {
    let pool = testing::pool().await;
    let account = testing::fresh_account(&pool, case).await;
    sqlx::query(
        "DELETE FROM stored_tracks WHERE track_id LIKE $1",
    )
    .bind(format!("{}-%", testing::scoped(case)))
    .execute(&pool)
    .await
    .expect("清账目失败");

    let emitted = Arc::new(AtomicU64::new(0));
    let url =
        serve_big_flac(emitted.clone(), with_length).await;
    let fake = FakeUpstream {
        play_source: Some(PlaySource {
            url,
            format: "flac".to_owned(),
            bit_rate: 4_000_000,
            level: QualityLevel::Lossless as i32,
            ..PlaySource::default()
        }),
        ..FakeUpstream::logged_in_with("42", vec![])
    };
    let gauge = Arc::new(Gauge {
        emitted,
        received: AtomicU64::new(0),
        peak_in_flight: AtomicU64::new(0),
        length: Mutex::new(None),
    });
    let state = AppState {
        archive: Some(Archive::new(gauge.clone())),
        ..testing::state(pool, testing::serve(fake).await)
    };
    (state, account, gauge)
}

/// 几百 MB 的一首照样存得进去,而在路上的字节始终有上界 —— 改回整首缓冲,
/// 峰值就是整首的大小,这条就挂。位深与采样率照样从流的开头读出来。
#[tokio::test]
async fn a_huge_track_streams_through_bounded_memory() {
    let (state, account, gauge) =
        big_fixture("ar_stream", true).await;
    let id = testing::track_id("ar_stream", 1);

    let outcome =
        store_track(&state, &account, &netease(&id)).await;

    assert_eq!(outcome, Ok(Stored::Now));
    assert_eq!(*gauge.length.lock().unwrap(), Some(TRACK));
    assert_eq!(
        gauge.received.load(Ordering::SeqCst),
        TRACK
    );
    let peak = gauge.peak_in_flight.load(Ordering::SeqCst);
    assert!(
        peak < IN_FLIGHT_CEILING,
        "在路上的字节峰值 {peak},上限 {IN_FLIGHT_CEILING}:整首被缓冲了"
    );
    let stored =
        row(&state, &id).await.expect("应当记了账");
    assert_eq!(
        (
            stored.bytes,
            stored.bits_per_sample,
            stored.sample_rate
        ),
        (TRACK as i64, Some(24), Some(96_000))
    );
}

/// 上游不给长度:单个 PUT 发不出去,这首不存、报错,留给队列重试 —— 而不是
/// 为了数长度把整首读进内存。
#[tokio::test]
async fn an_upstream_without_length_is_not_stored() {
    let (state, account, gauge) =
        big_fixture("ar_nolen", false).await;
    let id = testing::track_id("ar_nolen", 1);

    let outcome =
        store_track(&state, &account, &netease(&id)).await;

    assert!(outcome.is_err(), "{outcome:?}");
    assert_eq!(*gauge.length.lock().unwrap(), None);
    assert_eq!(row(&state, &id).await, None);
}
