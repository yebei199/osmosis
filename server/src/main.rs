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
    extract::FromRef,
    routing::{get, post},
};
use sqlx::PgPool;
use tonic::transport::Channel;
use tower_http::cors::CorsLayer;

use server::bangdream::proto::{
    auth_service_client::AuthServiceClient,
    catalog_service_client::CatalogServiceClient,
    discover_service_client::DiscoverServiceClient,
    library_service_client::LibraryServiceClient,
};
use server::error::Failure;
use server::signaling::{self, SharedRoster};
use server::{db, error};

mod routes;

use routes::auth::{health, login, logout, register};
use routes::history::{recent, record_play, stats};
use routes::likes::{
    like_track, liked, liked_ids, subscribe_playlist,
    unlike_track, unsubscribe_playlist,
};
use routes::lyric::lyric;
use routes::play::play;
use routes::playlists::{
    add_playlist_tracks, create_playlist, delete_playlist,
    platform_playlist_tracks, playlist_tracks, playlists,
    remove_playlist_tracks, rename_playlist,
};
use routes::search::{
    artist_tracks, daily, search_artists, search_playlists,
    search_tracks,
};

/// 默认监听地址。
///
/// 绑 `127.0.0.1` 而非 `0.0.0.0`:手机通过 `adb reverse tcp:3000 tcp:3000`
/// 把自己的 `127.0.0.1:3000` 转发到这里,不需要服务端暴露在局域网上。
const DEFAULT_BIND: &str = "127.0.0.1:3000";

/// bang-dream 聚合层的默认地址,与它的 `cmd/bang-dream` 默认监听一致。
const DEFAULT_UPSTREAM: &str = "http://127.0.0.1:50051";

/// 数据库连接串的默认值,与 `just pg` 起的容器一致。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

/// 四个 gRPC 客户端。共享同一条惰性连接,clone 只是加一份引用。
///
/// 拆成四个是 proto 的分服务结构决定的,不是本服务的设计 ——
/// 一次请求可能横跨其中几个(见 [`liked`])。
#[derive(Clone)]
pub(crate) struct Upstream {
    catalog: CatalogServiceClient<Channel>,
    library: LibraryServiceClient<Channel>,
    discover: DiscoverServiceClient<Channel>,
    auth: AuthServiceClient<Channel>,
}

/// 进程的全部共享状态。
///
/// 三样东西凑在一起只是因为 handler 需要它们,彼此之间没有关系:
/// 上游连接、自家的库、以及注册用的邀请码。
#[derive(Clone)]
pub(crate) struct AppState {
    upstream: Upstream,
    pool: PgPool,
    /// 注册时必须对上的邀请码,由环境变量 `INVITE_CODE` 给。
    invite: String,
}

// 鉴权提取器只要池,不该认识别的东西 —— 见 server::auth。
impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

/// 把 gRPC 失败翻成 HTTP 失败。
pub(crate) fn fail(status: &tonic::Status) -> Failure {
    let (code, body) = error::map_status(status);
    (code, Json(body))
}

/// 从池里取一条连接,失败翻成 HTTP 失败。
pub(crate) async fn conn(
    pool: &PgPool,
) -> Result<
    sqlx::pool::PoolConnection<sqlx::Postgres>,
    Failure,
> {
    pool.acquire()
        .await
        .map_err(|err| error::map_error(&err.into()))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let upstream = std::env::var("BANG_DREAM_ADDR")
        .unwrap_or_else(|_| DEFAULT_UPSTREAM.to_owned());
    // 惰性连接:bang-dream 没起来时本服务照样能启动,请求到来才失败并映射成 502。
    // 启动即连接的话,开发时两个进程的启动顺序会变成一条隐形约束。
    let channel = Channel::from_shared(upstream.clone())
        .expect("BANG_DREAM_ADDR 不是合法 URI")
        .connect_lazy();
    let clients = Upstream {
        catalog: CatalogServiceClient::new(channel.clone()),
        library: LibraryServiceClient::new(channel.clone()),
        discover: DiscoverServiceClient::new(
            channel.clone(),
        ),
        auth: AuthServiceClient::new(channel),
    };

    // 库连不上就不启动。惰性连上游是刻意的(见上),但数据库不同:
    // 没有它连登录都办不成,带着一个必然 500 的服务活着只会更难查。
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| {
            DEFAULT_DATABASE_URL.to_owned()
        });
    let pool = db::connect(&database_url)
        .await
        .expect("连接数据库或跑迁移失败");

    let state = AppState {
        upstream: clients,
        pool,
        invite: std::env::var("INVITE_CODE").expect(
            "必须设置 INVITE_CODE —— 没有它任何人都能注册",
        ),
    };

    // 同播信令。与音乐那几条路由**共用不了** state(一个是 gRPC 客户端、一个是
    // 在线名册),故各自 with_state 后再 merge —— 这也如实反映了两者毫无关系:
    // 信令不碰 bang-dream,音乐不碰 WebRTC。
    let signal = Router::new()
        .route("/signal", get(signaling::handler))
        .with_state(SharedRoster::default());

    let app = Router::new()
        .route("/health", get(health))
        // 账号三条不需要登录态 —— 它们正是用来取得登录态的
        .route("/register", post(register))
        .route("/login", post(login))
        // 登出要 token:它删的就是那一条会话
        .route("/logout", post(logout))
        // 三类搜索各一条路由:URL 与响应形状是同一个决定,不是两个要彼此对上的决定
        .route("/search/tracks", get(search_tracks))
        .route("/search/artists", get(search_artists))
        .route("/search/playlists", get(search_playlists))
        // 搜到的歌手点下去听什么 —— 平台此刻认为的热门那几首
        .route("/artists/{id}/tracks", get(artist_tracks))
        .route("/daily", get(daily))
        .route("/liked", get(liked))
        // 红心的**全量标识**,不分页。/liked 给的是一页曲目,回答不了
        // 「这一首红心没有」—— 而界面每一行都要问这个问题。
        .route("/liked/ids", get(liked_ids))
        // 红心与收藏各用自己的名词,不挂在 /playlists/{id} 下:
        // 那条路径的 id 是本地歌单的整数主键,而收藏的是平台歌单的字符串 id ——
        // 同一个 {id} 指两个 id 空间,迟早有人传错一个
        .route(
            "/liked/{track_id}",
            axum::routing::put(like_track)
                .delete(unlike_track),
        )
        .route(
            "/subscriptions/playlists/{playlist_id}",
            axum::routing::put(subscribe_playlist)
                .delete(unsubscribe_playlist),
        )
        .route(
            "/playlists",
            get(playlists).post(create_playlist),
        )
        // 路径里带上来源,因为两种歌单的 id **不在同一个空间**:本地是整数主键,
        // 平台是平台自己的字符串 id。挤在同一个 `{id}` 下的话,迟早传错一个,
        // 而那时的现象是「查无此歌单」——看起来像数据没了。
        .route(
            "/playlists/local/{id}",
            axum::routing::patch(rename_playlist)
                .delete(delete_playlist),
        )
        .route(
            "/playlists/local/{id}/tracks",
            get(playlist_tracks)
                .post(add_playlist_tracks)
                .delete(remove_playlist_tracks),
        )
        .route(
            "/playlists/platform/{id}/tracks",
            get(platform_playlist_tracks),
        )
        .route("/play/{track_id}", get(play))
        .route("/lyric/{track_id}", get(lyric))
        .route("/played", post(record_play))
        .route("/recent", get(recent))
        .route("/stats", get(stats))
        .with_state(state)
        .merge(signal)
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

    tracing::info!(%bind, %upstream, "服务已启动");
    axum::serve(listener, app)
        .await
        .expect("server failed");
}
