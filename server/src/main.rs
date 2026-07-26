//! 后端服务。
//!
//! 两个职责:与客户端共享 [`contract`] crate(线上格式的一致性由编译器保证 ——
//! 改了 DTO 而忘了改另一侧,构建会直接失败),以及把 bang-dream 聚合层的 gRPC
//! 翻译成客户端要的 HTTP/JSON。
//!
//! gRPC 只存在于 [`bangdream`] 模块内部。客户端不认识 gRPC,也不必为上游 proto
//! 的演化重新编译 —— 那正是这层转发买到的东西。
//!
//! 运行:`just server-dev`(等价于 `cargo run -p server`)。
//! 需要 bang-dream 在另一个终端里跑着,见 `just bang-dream`。
//!
//! 注意 workspace 的 `default-members` 不含本 crate,裸 `cargo build` 不会编它。

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use contract::{
    ErrorDto, HealthDto, PROTOCOL_VERSION, PlaySourceDto,
    SearchDto,
};
use serde::Deserialize;
use tonic::transport::Channel;
use tower_http::cors::CorsLayer;

use server::bangdream::{
    self,
    proto::{
        GetPlaySourceRequest, Platform, QualityLevel,
        SearchTracksRequest,
        catalog_service_client::CatalogServiceClient,
    },
};
use server::error;

/// 默认监听地址。
///
/// 绑 `127.0.0.1` 而非 `0.0.0.0`:手机通过 `adb reverse tcp:3000 tcp:3000`
/// 把自己的 `127.0.0.1:3000` 转发到这里,不需要服务端暴露在局域网上。
const DEFAULT_BIND: &str = "127.0.0.1:3000";

/// bang-dream 聚合层的默认地址,与它的 `cmd/bang-dream` 默认监听一致。
const DEFAULT_UPSTREAM: &str = "http://127.0.0.1:50051";

/// 搜索默认返回条数。
const DEFAULT_SEARCH_LIMIT: i32 = 30;

/// 取播放地址时请求的音质档位。
///
// ponytail: 先写死。做到音质选择时再提成查询参数 —— 现在没有任何界面能选它。
const PLAY_QUALITY: QualityLevel = QualityLevel::High;

/// 失败响应:状态码 + [`ErrorDto`]。
type Failure = (StatusCode, Json<ErrorDto>);

/// 把 gRPC 失败翻成 HTTP 失败。
fn fail(status: &tonic::Status) -> Failure {
    let (code, body) = error::map_status(status);
    (code, Json(body))
}

#[tokio::main]
async fn main() {
    let upstream = std::env::var("BANG_DREAM_ADDR")
        .unwrap_or_else(|_| DEFAULT_UPSTREAM.to_owned());
    // 惰性连接:bang-dream 没起来时本服务照样能启动,请求到来才失败并映射成 502。
    // 启动即连接的话,开发时两个进程的启动顺序会变成一条隐形约束。
    let channel = Channel::from_shared(upstream.clone())
        .expect("BANG_DREAM_ADDR 不是合法 URI")
        .connect_lazy();
    let catalog = CatalogServiceClient::new(channel);

    let app = Router::new()
        .route("/health", get(health))
        .route("/search", get(search))
        .route("/play/{track_id}", get(play))
        .with_state(catalog)
        // 浏览器把 `localhost:3000` 视为跨源,wasm 端不开 CORS 连不上。
        // permissive 只适用于开发:它允许任意来源。
        .layer(CorsLayer::permissive());

    let bind = std::env::var("BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| {
            panic!("failed to bind {bind}: {e}")
        });

    println!(
        "listening on http://{bind}, upstream {upstream}"
    );
    axum::serve(listener, app)
        .await
        .expect("server failed");
}

/// `GET /health` —— 能返回就说明服务端活着。
///
/// 它**不**探测 bang-dream:这里回答的是"本服务活着吗",上游是否可用
/// 由真正用到它的请求各自报告。混在一起的话,上游一挂客户端会以为后端整个死了。
async fn health() -> Json<HealthDto> {
    Json(HealthDto {
        status: "ok".to_owned(),
        protocol_version: PROTOCOL_VERSION,
    })
}

/// `GET /search` 的查询参数。
#[derive(Deserialize)]
struct SearchQuery {
    /// 关键词。
    q: String,
    /// 每页条数,不给按 [`DEFAULT_SEARCH_LIMIT`]。
    limit: Option<i32>,
    /// 偏移量,不给从 0 开始。翻页由客户端自行推进。
    offset: Option<i32>,
}

/// `GET /search?q=紅蓮華` —— 搜歌。
async fn search(
    State(mut catalog): State<
        CatalogServiceClient<Channel>,
    >,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchDto>, Failure> {
    let response = catalog
        .search_tracks(SearchTracksRequest {
            platform: Platform::Netease as i32,
            keyword: query.q,
            limit: query
                .limit
                .unwrap_or(DEFAULT_SEARCH_LIMIT),
            offset: query.offset.unwrap_or_default(),
        })
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(SearchDto {
        tracks: response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
        has_more: response.has_more,
    }))
}

/// `GET /play/{track_id}` —— 取一条临时直链。
///
/// 每次都向上游重新要:直链带签名会过期,缓存它只会让客户端拿到放不出声的地址。
async fn play(
    State(mut catalog): State<
        CatalogServiceClient<Channel>,
    >,
    Path(track_id): Path<String>,
) -> Result<Json<PlaySourceDto>, Failure> {
    let response = catalog
        .get_play_source(GetPlaySourceRequest {
            platform: Platform::Netease as i32,
            track_id,
            level: PLAY_QUALITY as i32,
        })
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // source 缺席意味着上游认为拿到了、却没给内容 —— 当成上游失败,不静默返回空。
    let source = response.source.ok_or_else(|| {
        fail(&tonic::Status::internal("上游没有返回播放源"))
    })?;

    Ok(Json(bangdream::play_source_to_dto(source)))
}
