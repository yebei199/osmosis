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
    extract::{DefaultBodyLimit, FromRef},
    http::HeaderValue,
    routing::{get, post},
};
use sqlx::PgPool;
use tonic::transport::Channel;
use tower_governor::GovernorLayer;
use tower_http::cors::{Any, CorsLayer};

use server::bangdream::proto::{
    auth_service_client::AuthServiceClient,
    catalog_service_client::CatalogServiceClient,
    discover_service_client::DiscoverServiceClient,
    library_service_client::LibraryServiceClient,
};
use server::bangdream::{Timed, UpstreamChannel};
use server::error;
use server::error::Failure;
use server::gate::ratelimit::{self, Policies};
use server::objects::{S3, S3Config};
use server::store::db;
use server::syncplay::signaling::{
    self, AllowedOrigins, SharedRoster,
};

mod routes;

use routes::auth::{health, login, logout, register};
use routes::catalog::catalog_cache::Freshness;
use routes::catalog::lyric::lyric;
use routes::catalog::netease::{
    create_qr, qr_state, status as netease_status, unbind,
};
use routes::catalog::search::{
    artist_tracks, daily, search_artists, search_playlists,
    search_tracks,
};
use routes::library::history::{
    recent, record_play, stats,
};
use routes::library::likes::{
    import_liked, like_track, liked, liked_ids,
    subscribe_playlist, unlike_track, unsubscribe_playlist,
};
use routes::library::playlists::{
    add_playlist_tracks, create_playlist, delete_playlist,
    platform_playlist_tracks, playlist_tracks, playlists,
    remove_playlist_tracks, rename_playlist,
};
use routes::play::archive::Archive;
use routes::play::download::download;
use routes::play::links::SignedLinks;
use routes::play::play;
use routes::queue::{
    create_queue, publish_queue, queue_head, queue_page,
    report_queue_state, set_queue_intent,
};

/// 默认监听地址。
///
/// 绑 `127.0.0.1` 而非 `0.0.0.0`:手机通过 `adb reverse tcp:3000 tcp:3000`
/// 把自己的 `127.0.0.1:3000` 转发到这里,不需要服务端暴露在局域网上。
const DEFAULT_BIND: &str = "127.0.0.1:3000";

/// bang-dream 聚合层的默认地址,与它的 `cmd/bang-dream` 默认监听一致。
const DEFAULT_UPSTREAM: &str = "http://127.0.0.1:50051";

/// 浏览器来源白名单的默认值,与 `just web-dev` 的静态服务器一致。
///
/// 两条都要:`127.0.0.1` 与 `localhost` 是**不同的来源**,浏览器不会把它们
/// 当作一回事。部署时用环境变量 `CORS_ORIGINS` 覆盖(逗号分隔)。
const DEFAULT_CORS_ORIGINS: &str =
    "http://127.0.0.1:8073,http://localhost:8073";

/// 数据库连接串的默认值,与 `just pg` 起的容器一致。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

/// 四个 gRPC 客户端。共享同一条惰性连接,clone 只是加一份引用。
///
/// 拆成四个是 proto 的分服务结构决定的,不是本服务的设计 ——
/// 一次请求可能横跨其中几个(见 [`import_liked`])。
#[derive(Clone)]
pub(crate) struct Upstream {
    catalog: CatalogServiceClient<UpstreamChannel>,
    library: LibraryServiceClient<UpstreamChannel>,
    discover: DiscoverServiceClient<UpstreamChannel>,
    auth: AuthServiceClient<UpstreamChannel>,
}

/// 进程的全部共享状态。
///
/// 四样东西凑在一起只是因为 handler 需要它们,彼此之间没有关系:
/// 上游连接、自家的库、注册用的邀请码,以及信令的在线名册。
#[derive(Clone)]
pub(crate) struct AppState {
    upstream: Upstream,
    pool: PgPool,
    /// 注册时必须对上的邀请码,由环境变量 `INVITE_CODE` 给。
    invite: String,
    /// 信令的在线名册。与音乐那几条路由毫无关系,只是同住一个进程 ——
    /// 但 `/signal` 要鉴权,而鉴权提取器要池,两者因此必须在同一份 state 里。
    roster: SharedRoster,
    /// 浏览器来源白名单。CORS 与 WebSocket 的 Origin 校验共用这一张表 ——
    /// 配两份的话,迟早只改了一处,而那时 web 端会在其中一道门上莫名其妙地失败。
    origins: AllowedOrigins,
    /// 六条限流策略,构造一次共享 `Arc`(见 `gate::ratelimit`)。
    policies: Policies,
    /// 库里每个平台歌单的那份是什么时候回源拿到的(见 `catalog_cache`)。
    playlists: Freshness,
    /// 上游直链在有效期内的那一份(见 `routes::play::links`)。
    links: SignedLinks,
    /// 安卓安装包的回源地址(见 `routes::apk`)。
    apk_releases: String,
    /// 听过的歌存到哪(#126)。没配 `S3_ENDPOINT` 就是 `None`,整套归档不启用 ——
    /// 本机开发不必为它起一个 RustFS。
    archive: Option<Archive>,
}

// 鉴权提取器只要池,不该认识别的东西 —— 见 server::gate::auth。
impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

// 信令 handler 只要名册,同理。
impl FromRef<AppState> for SharedRoster {
    fn from_ref(state: &AppState) -> Self {
        state.roster.clone()
    }
}

impl FromRef<AppState> for AllowedOrigins {
    fn from_ref(state: &AppState) -> Self {
        state.origins.clone()
    }
}

impl FromRef<AppState> for Policies {
    fn from_ref(state: &AppState) -> Self {
        state.policies.clone()
    }
}

/// 浏览器来源白名单,由环境变量 `CORS_ORIGINS` 给,逗号分隔。
fn allowed_origins() -> Vec<String> {
    std::env::var("CORS_ORIGINS")
        .unwrap_or_else(|_| DEFAULT_CORS_ORIGINS.to_owned())
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
        .collect()
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

/// 一次队列上传最多多大。
///
/// 现场那份 977 首的 `/liked` 响应体是 224142 字节,合每首约 229 字节;
/// 五千首(`MAX_QUEUE_ENTRIES`)按这个密度约 1.1 MB,标题与歌手长一些的
/// 翻一倍也就 2 MB 出头。axum 默认 2 MiB **正好卡在这个量级上** ——
/// 默认值不是产品预算,所以显式给 8 MiB,留够余量而不是留够刚好。
const QUEUE_UPLOAD_LIMIT: usize = 8 * 1024 * 1024;

/// 执行报告最多多大。
///
/// 它带一个 `play_order`:五千个 `entry_id` 的 JSON 约 30 KB。256 KiB 绰绰有余,
/// 而把它与上传分开的理由是**小操作不该接受大 body** —— 一条本该几百字节的
/// 请求收下 8 MB,那是一条白送的放大路径。
const REPORT_BODY_LIMIT: usize = 256 * 1024;

/// 小操作的 body 上限:意图、登录、注册。
const SMALL_BODY_LIMIT: usize = 16 * 1024;

/// 队列那六条,按**策略分组**挂限流。
///
/// 每组 `route_layer` 两层,顺序要紧:鉴权在外、限流在内。反过来的话限流
/// 先跑,而那时 extensions 里还没有账号,`AccountKey` 取不到键。
///
/// 用 `route_layer` 而不是 `layer`:后者会套到**没匹配上的请求**上,于是
/// 一个打错的 URL 也要先过一遍限流,404 变成 429。
fn queue_routes(state: &AppState) -> Router {
    let write = Router::new()
        .route("/queues", post(create_queue))
        .route(
            "/queues/{id}/revisions",
            post(publish_queue),
        )
        .layer(DefaultBodyLimit::max(QUEUE_UPLOAD_LIMIT))
        .route_layer(guard(
            state.policies.queue_write.clone(),
        ))
        .route_layer(authenticated(state));

    let intent = Router::new()
        .route(
            "/queues/{id}/intent",
            post(set_queue_intent),
        )
        .layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT))
        .route_layer(guard(
            state.policies.queue_intent.clone(),
        ))
        .route_layer(authenticated(state));

    let report = Router::new()
        .route(
            "/queues/{id}/report",
            post(report_queue_state),
        )
        .layer(DefaultBodyLimit::max(REPORT_BODY_LIMIT))
        .route_layer(guard(
            state.policies.queue_report.clone(),
        ))
        .route_layer(authenticated(state));

    let read = Router::new()
        .route("/queues/{id}", get(queue_page))
        .route("/queues/{id}/head", get(queue_head))
        .route_layer(guard(
            state.policies.queue_read.clone(),
        ))
        .route_layer(authenticated(state));

    Router::new()
        .merge(write)
        .merge(intent)
        .merge(report)
        .merge(read)
        .with_state(state.clone())
}

/// 组的全局播放状态(#142)。点歌可能带着整批曲目,与建队列同一道闸、同一个 body 上限;
/// 其余几条是小操作,与队列意图同一道闸。
fn group_routes(state: &AppState) -> Router {
    let play = Router::new()
        .route("/group/play", post(routes::group::play))
        .layer(DefaultBodyLimit::max(QUEUE_UPLOAD_LIMIT))
        .route_layer(guard(
            state.policies.queue_write.clone(),
        ))
        .route_layer(authenticated(state));

    let steer = Router::new()
        .route(
            "/group/transport",
            post(routes::group::transport),
        )
        .route(
            "/group/outputs",
            post(routes::group::outputs),
        )
        .route("/group/leave", post(routes::group::leave))
        .route(
            "/group/advance",
            post(routes::group::advance),
        )
        .layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT))
        .route_layer(guard(
            state.policies.queue_intent.clone(),
        ))
        .route_layer(authenticated(state));

    let read = Router::new()
        .route("/group", get(routes::group::current))
        .route_layer(guard(
            state.policies.queue_read.clone(),
        ))
        .route_layer(authenticated(state));

    Router::new()
        .merge(play)
        .merge(steer)
        .merge(read)
        .with_state(state.clone())
}

/// 信令建连。
///
/// **这一层只拦升级请求本身**,拦不到升级之后那条 WebSocket 上的消息 ——
/// 那一侧的上限仍是 `signaling::MAX_MESSAGE_BYTES`,两道闸各管各的。
fn signal_routes(state: &AppState) -> Router {
    Router::new()
        .route("/signal", get(signaling::handler))
        .route_layer(guard(
            state.policies.signal_connect.clone(),
        ))
        .route_layer(authenticated(state))
        .with_state(state.clone())
}

/// 登录与注册。**按来源 IP** —— 它们正是用来取得登录态的,那时还没有账号,
/// 所以这一组不挂鉴权前置。
fn auth_routes(state: &AppState) -> Router {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT))
        .route_layer(
            GovernorLayer::new(
                state.policies.auth_attempt.clone(),
            )
            .error_handler(ratelimit::too_many_requests),
        )
        .with_state(state.clone())
}

/// 按账号分桶的那道闸,配上本仓自己的错误形状。
fn guard(
    policy: std::sync::Arc<
        tower_governor::governor::GovernorConfig<
            ratelimit::AccountKey,
            governor::middleware::NoOpMiddleware,
        >,
    >,
) -> GovernorLayer<
    ratelimit::AccountKey,
    governor::middleware::NoOpMiddleware,
    axum::body::Body,
> {
    GovernorLayer::new(policy)
        .error_handler(ratelimit::too_many_requests)
}

/// 每个请求一行总耗时,并给它开一个带 id 的 span(#121)。
///
/// 请求里发生的上游调用(`bangdream::Timed`)打在这个 span 下,同一个 id 的
/// 几行就是这一个请求的分段:上游花了多久、总共多久,差额是库与本服务自己。
/// 路由取匹配上的模板(`/playlists/platform/{id}/tracks`),路径里的 id 与
/// 查询串都不进日志。
async fn timed_request(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use tracing::Instrument as _;

    // ponytail: 进程级计数器只用来发号,不承载别的状态
    static NEXT_ID: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(1);
    let id = NEXT_ID
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or("?", axum::extract::MatchedPath::as_str)
        .to_owned();
    let span = tracing::info_span!(
        "req",
        id,
        method = %request.method(),
        %route,
    );

    let started = std::time::Instant::now();
    let response =
        next.run(request).instrument(span.clone()).await;
    span.in_scope(|| {
        tracing::info!(
            status = response.status().as_u16(),
            ms = started.elapsed().as_millis(),
            "done"
        );
    });
    response
}

/// 前置鉴权:跑一遍 `Account` 提取器,把认下来的账号放进 extensions。
///
/// 限流在中间件层跑、拿不到提取器的返回值,所以只能这样把账号递给它。
/// handler 上那个 `Account` 参数会读到同一份(见 `gate::auth`),不会再打
/// 一次库。
fn authenticated(
    state: &AppState,
) -> axum::middleware::FromExtractorLayer<
    server::store::account::Account,
    AppState,
> {
    axum::middleware::from_extractor_with_state::<
        server::store::account::Account,
        AppState,
    >(state.clone())
}

/// 按环境变量装配对象存储(见 `server::objects::S3Config::from_env`)。
fn archive() -> Option<Archive> {
    let Some(config) = S3Config::from_env() else {
        tracing::info!("没设 S3_ENDPOINT,听过的歌不存");
        return None;
    };
    tracing::info!(endpoint = %config.endpoint, bucket = %config.bucket, "听过的歌存进对象存储");
    let s3 = S3::new(config).expect("S3 配置不对");
    Some(Archive::new(std::sync::Arc::new(s3)))
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
    let channel = Timed(channel);
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
        roster: SharedRoster::default(),
        origins: AllowedOrigins::new(allowed_origins()),
        policies: Policies::tuned(),
        playlists: Freshness::default(),
        links: SignedLinks::default(),
        apk_releases: std::env::var("APK_RELEASES_BASE")
            .unwrap_or_else(|_| {
                routes::apk::DEFAULT_RELEASES_BASE
                    .to_owned()
            }),
        archive: archive(),
    };
    // 组放完一首就往下推一首(#142)。
    server::syncplay::group::spawn_roller(
        state.pool.clone(),
        state.roster.clone(),
    );
    // 久未出现的键要定期清掉,否则这几张表只涨不落。
    state.policies.spawn_cleanup();
    // 没人红心、三天没播的存歌同理
    if let Some(archive) = &state.archive {
        routes::play::archive::spawn_sweeper(
            state.pool.clone(),
            archive,
        );
    }
    // 歌单与日推里的歌以无损预先存进桶(#147)
    routes::play::prefetch::spawn(
        &state,
        routes::play::prefetch::Limits::from_env(),
    );

    let app = Router::new()
        .route("/health", get(health))
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
        // 把网易云的红心并进「我的喜欢」:只补新增,重跑不重复。界面上没有按钮,
        // 手动用 curl 打(#146)。静态段优先于 /liked/{track_id},曲目 id 是数字,撞不上
        .route("/liked/import", post(import_liked))
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
        // 网易云绑定。凭据按账号分片(docs/adr/0017),所以这几条都要登录态 ——
        // 提取器给出的账号正是上游用来分片的那个键。
        .route("/netease/status", get(netease_status))
        .route("/netease", axum::routing::delete(unbind))
        .route("/netease/qr", post(create_qr))
        // 轮询一次当前态。上游那条是 server stream,长连接留在服务端这一侧:
        // 客户端两端都只有 get_json 一种传输(见 routes::netease)。
        .route("/netease/qr/{key}", get(qr_state))
        .route("/play/{track_id}", get(play))
        // 下载与播放分开两条路由:播放交出一条直链让客户端自己去取,下载把字节
        // 拉过来并归一成 mp3。挤成一条带 `?download=1` 的话,响应体的**类型**
        // 会随参数变(JSON 还是音频流),而那是两件事,不是一件事的两个选项。
        .route("/download/{track_id}", get(download))
        .route("/lyric/{track_id}", get(lyric))
        // 应用内升级的安装包(#129)。版本与哈希客户端直接问 GitHub,只有字节从这里过。
        .route(
            "/app/android/{file}",
            get(routes::apk::android_apk),
        )
        .route("/played", post(record_play))
        .route("/recent", get(recent))
        .route("/stats", get(stats))
        .with_state(state.clone())
        // 队列与信令各自成组,好把限流挂在组上(见下面那几个构造函数)。
        .merge(queue_routes(&state))
        .merge(group_routes(&state))
        .merge(signal_routes(&state))
        .merge(auth_routes(&state))
        // 每个请求一行耗时,并给它的上游调用开一个共同的 span(见 timed_request)。
        // 挂在所有路由组合并之后,队列、信令、登录那几组也一并算进去。
        .layer(axum::middleware::from_fn(timed_request))
        // 浏览器把 `localhost:3000` 视为跨源,wasm 端不开 CORS 连不上。
        // 白名单而不是 permissive:后者允许任意来源,等于任何网页都能拿着
        // 用户的登录态调这些路由。方法与请求头仍然放开 —— 没开 credentials,
        // 凭据只会是客户端自己塞进 Authorization 头的那一个。
        .layer(
            CorsLayer::new()
                .allow_origin(
                    state
                        .origins
                        .iter()
                        .filter_map(|origin| {
                            origin.parse().ok()
                        })
                        .collect::<Vec<HeaderValue>>(),
                )
                .allow_methods(Any)
                .allow_headers(Any),
        );

    let bind = std::env::var("BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|e| {
            panic!("failed to bind {bind}: {e}")
        });

    tracing::info!(%bind, %upstream, "服务已启动");
    // 带上连接信息:登录与注册按来源 IP 限流,而 `ConnectInfo` 只有这样装配
    // 才取得到。**反代之后这个 IP 是代理的** —— 真实客户端 IP 在
    // `X-Forwarded-For` 里,而无条件信任那个头比不限流更糟(谁都能伪造它)。
    // 要按真实 IP 限流得先决定信任哪一层代理,那是部署侧的决定。
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("server failed");
}

#[cfg(test)]
mod timing_tests {
    use std::sync::{Arc, Mutex};

    use axum::http::{Request, Response};
    use axum::routing::get;
    use server::bangdream::Timed;
    use tower::Service as _;

    use super::timed_request;

    /// 把 tracing 的输出攒进一块共享缓冲。
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Sink {
        fn write(
            &mut self,
            bytes: &[u8],
        ) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| {
                    poisoned.into_inner()
                })
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 一个立刻回 200 的假上游,只为让 `Timed` 有东西可包。
    #[derive(Clone)]
    struct Answering;

    impl<B> tower::Service<Request<B>> for Answering {
        type Response = Response<String>;
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<
            Result<Self::Response, Self::Error>,
        >;

        fn poll_ready(
            &mut self,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>>
        {
            std::task::Poll::Ready(Ok(()))
        }

        fn call(&mut self, _: Request<B>) -> Self::Future {
            std::future::ready(Ok(Response::new(
                String::new(),
            )))
        }
    }

    /// 行里 `req{id=N` 那一段 —— 同一个请求的几行靠它串起来。
    fn request_id(line: &str) -> Option<&str> {
        let start = line.find("req{id=")?;
        let rest = &line[start..];
        Some(&rest[..rest.find(' ')?])
    }

    /// 一个请求打一行总耗时,它发出的上游调用打在同一个 id 下;
    /// 路径里的 id 与查询串不进日志。
    ///
    /// 这是回答「慢在上游还是慢在我们」的那组数:少了 id,并发的几个请求
    /// 各自的上游耗时就分不开了。
    #[tokio::test]
    async fn a_request_and_its_upstream_calls_share_one_id()
    {
        let sink = Sink::default();
        let writer = sink.clone();
        // 再挂一个活着的 dispatcher,只为让进程里的 dispatcher 多于一个。
        //
        // 只有一个时,tracing-core 走 `has_just_one` 捷径:别的线程**第一次**碰到
        // 某个埋点,按那个线程自己的默认(NoSubscriber)算兴趣,把 never 缓存成全局的。
        // 其余路由测试在别的线程上也走 `Timed`,赶在本测试开着的那一刻首次碰到
        // "upstream" 那行,本测试就再也收不到它 —— 时灵时不灵,六次里挂两次。
        // 多于一个时,注册改为问遍登记在册的 dispatcher,其中就有下面这一个。
        let _second = tracing::Dispatch::new(
            tracing_subscriber::registry(),
        );
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish(),
        );

        let app = axum::Router::new()
            .route(
                "/probe/{id}",
                get(|| async {
                    let request = Request::builder()
                        .uri("http://upstream/bangdream.music.v1.CatalogService/GetTracks")
                        .body(())
                        .expect("拼不出假请求");
                    let _ = Timed(Answering).call(request).await;
                    "ok"
                }),
            )
            .layer(axum::middleware::from_fn(timed_request));
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("绑不上本地端口");
        let addr =
            listener.local_addr().expect("取不到本地地址");
        tokio::spawn(async move {
            axum::serve(listener, app).await
        });

        let status = reqwest::get(format!(
            "http://{addr}/probe/7?q=secret"
        ))
        .await
        .expect("请求没发出去")
        .status();
        assert_eq!(status, 200);

        let log = String::from_utf8_lossy(
            &sink.0.lock().unwrap_or_else(|poisoned| {
                poisoned.into_inner()
            }),
        )
        .into_owned();
        let upstream: Vec<&str> = log
            .lines()
            .filter(|line| {
                line.contains("CatalogService/GetTracks")
            })
            .collect();
        let done: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("status=200"))
            .collect();
        assert_eq!(
            (upstream.len(), done.len()),
            (1, 1),
            "{log}"
        );
        assert!(upstream[0].contains("ms="), "{log}");
        assert!(done[0].contains("ms="), "{log}");
        assert!(done[0].contains("/probe/{id}"), "{log}");
        assert!(request_id(done[0]).is_some(), "{log}");
        assert_eq!(
            request_id(upstream[0]),
            request_id(done[0]),
            "{log}"
        );
        assert!(
            !log.contains("secret")
                && !log.contains("/probe/7"),
            "{log}"
        );
    }
}
