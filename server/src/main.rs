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
    extract::{FromRef, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use contract::{
    HealthDto, LoginDto, LyricDto, PROTOCOL_VERSION,
    PlaySourceDto, PlaylistDto, PlaylistsDto, RegisterDto,
    SearchDto, SessionDto, TracksDto,
};
use serde::Deserialize;
use sqlx::PgPool;
use tonic::transport::Channel;
use tower_http::cors::CorsLayer;

use server::account::{self, Account};
use server::bangdream::{
    self,
    proto::{
        GetAccountStatusRequest,
        GetDailyRecommendationsRequest, GetLyricRequest,
        GetPlaySourceRequest, GetTracksRequest,
        ListLikedTracksRequest, ListUserPlaylistsRequest,
        Platform, QualityLevel, SearchTracksRequest,
        auth_service_client::AuthServiceClient,
        catalog_service_client::CatalogServiceClient,
        discover_service_client::DiscoverServiceClient,
        library_service_client::LibraryServiceClient,
    },
};
use server::error::Failure;
use server::playlist::{self, TrackRef};
use server::signaling::{self, SharedRoster};
use server::{db, error, paging};

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

/// 数据库连接串的默认值,与 `just pg` 起的容器一致。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/slint_study";

/// 四个 gRPC 客户端。共享同一条惰性连接,clone 只是加一份引用。
///
/// 拆成四个是 proto 的分服务结构决定的,不是本服务的设计 ——
/// 一次请求可能横跨其中几个(见 [`liked`])。
#[derive(Clone)]
struct Upstream {
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
struct AppState {
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
fn fail(status: &tonic::Status) -> Failure {
    let (code, body) = error::map_status(status);
    (code, Json(body))
}

/// 从池里取一条连接,失败翻成 HTTP 失败。
async fn conn(
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
        .route("/search", get(search))
        .route("/daily", get(daily))
        .route("/liked", get(liked))
        .route(
            "/playlists",
            get(playlists).post(create_playlist),
        )
        .route(
            "/playlists/{id}",
            axum::routing::patch(rename_playlist)
                .delete(delete_playlist),
        )
        .route(
            "/playlists/{id}/tracks",
            get(playlist_tracks)
                .post(add_playlist_tracks)
                .delete(remove_playlist_tracks),
        )
        .route("/play/{track_id}", get(play))
        .route("/lyric/{track_id}", get(lyric))
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

/// `POST /register` —— 凭邀请码开一个账号,并直接给出可用的会话。
///
/// 注册完顺手登录:否则客户端要连发两次请求,而中间那个"注册成功但没登录"的
/// 状态没有任何用处。
async fn register(
    State(state): State<AppState>,
    Json(body): Json<RegisterDto>,
) -> Result<Json<SessionDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let created = account::register(
        &mut conn,
        &body.username,
        &body.password,
        &body.invite,
        &state.invite,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    let token = account::login(
        &mut conn,
        &body.username,
        &body.password,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(Json(SessionDto {
        token,
        username: created.username,
    }))
}

/// `POST /login` —— 用用户名密码换一个会话 token。
async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginDto>,
) -> Result<Json<SessionDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let token = account::login(
        &mut conn,
        &body.username,
        &body.password,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    // 回显规范化后的账号名(登录时大小写不敏感),界面据此显示
    let account = account::authenticate(&mut conn, &token)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(SessionDto {
        token,
        username: account.username,
    }))
}

/// `POST /logout` —— 吊销**这一条**会话,别处登录的 token 不受影响。
///
/// 参数里的 `Account` 不是摆设:它保证了只有持有效 token 的人能走到这里。
/// 要删的 token 从请求头再取一次 —— 提取器把它换成了账号,没有留下原文。
async fn logout(
    State(state): State<AppState>,
    _account: Account,
    headers: HeaderMap,
) -> Result<StatusCode, Failure> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| {
            error::unauthorized("缺少 Authorization 头")
        })?;

    let mut conn = conn(&state.pool).await?;
    account::logout(&mut conn, token)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
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
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .search_tracks(bangdream::as_user(
            &account,
            SearchTracksRequest {
                platform: Platform::Netease as i32,
                keyword: query.q,
                limit: query
                    .limit
                    .unwrap_or(DEFAULT_SEARCH_LIMIT),
                offset: query.offset.unwrap_or_default(),
            },
        ))
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

/// `GET /daily` —— 今日推荐。
///
/// 上游直接给完整曲目,不像 [`liked`] 那样只给标识。
async fn daily(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TracksDto>, Failure> {
    let mut discover = state.upstream.discover;
    let response = discover
        .get_daily_recommendations(bangdream::as_user(
            &account,
            GetDailyRecommendationsRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(TracksDto {
        tracks: response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
    }))
}

/// `GET /liked` 的查询参数。
#[derive(Deserialize)]
struct PageQuery {
    limit: Option<usize>,
    offset: Option<usize>,
}

/// `GET /liked?limit=&offset=` —— 我喜欢的音乐。
///
/// 三步:先问上游当前账号是谁,再拿它**全量**的红心标识列表,切一页出来补全成曲目。
/// 上游只给标识是刻意的(它的 `docs/adr/0003`):平台返回的曲目列表会被截断,
/// 标识列表不会,翻页因此由调用方持有列表自行完成,聚合层不必缓存。
///
/// user_id 不做缓存:重新扫码登录会换一个账号,缓存住的话红心列表会静默停在旧账号上。
/// 这是一次同机 gRPC,便宜得没必要省。
async fn liked(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<PageQuery>,
) -> Result<Json<TracksDto>, Failure> {
    let mut auth = state.upstream.auth;
    let mut library = state.upstream.library;
    let mut catalog = state.upstream.catalog;

    let netease_account = auth
        .get_account_status(bangdream::as_user(
            &account,
            GetAccountStatusRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // 未登录是**状态**不是错误,所以上游用 logged_in 而非错误码回答(它的 `docs/adr/0005`)。
    // 但对这个请求而言目的没达成 —— 返回空列表会被读成"一首喜欢的都没有",
    // 那是另一件事,必须区分开。
    if !netease_account.logged_in {
        return Err(fail(&tonic::Status::unauthenticated(
            "netease: 未登录",
        )));
    }

    let liked = library
        .list_liked_tracks(bangdream::as_user(
            &account,
            ListLikedTracksRequest {
                platform: Platform::Netease as i32,
                user_id: netease_account.user_id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    let ids = paging::page(
        &liked.track_ids,
        query.offset.unwrap_or_default(),
        query.limit.unwrap_or_default(),
    );
    if ids.is_empty() {
        return Ok(Json(TracksDto { tracks: Vec::new() }));
    }

    let response = catalog
        .get_tracks(bangdream::as_user(
            &account,
            GetTracksRequest {
                platform: Platform::Netease as i32,
                track_ids: ids.to_vec(),
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(TracksDto {
        tracks: response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
    }))
}

/// `GET /playlists` —— 两个来源合成的一张歌单列表。
///
/// 平台歌单直读上游、不镜像;本地歌单读自家的库;「我喜欢的」置顶,它就是
/// 平台的红心列表(见 `docs/adr/0016`)。
///
/// 上游要不到平台歌单时**不整个失败**:本地那半与红心仍然有用,把它们一起
/// 扣下等于让网易云的一次抖动把用户自己的歌单也弄没了。
async fn playlists(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<PlaylistsDto>, Failure> {
    let mut auth = state.upstream.auth;
    let mut library = state.upstream.library;

    let netease_account = auth
        .get_account_status(bangdream::as_user(
            &account,
            GetAccountStatusRequest {
                platform: Platform::Netease as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    let (platform, liked_count) =
        if netease_account.logged_in {
            platform_playlists(
                &mut library,
                &account,
                &netease_account.user_id,
            )
            .await
        } else {
            // 没绑网易云是**状态**不是错误:本地歌单照常给,列表里就是少了平台那部分
            (Vec::new(), 0)
        };

    let mut conn = conn(&state.pool).await?;
    let local = playlist::list(&mut conn, account.id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(PlaylistsDto {
        playlists: playlist::merged(
            liked_count,
            platform,
            local,
        ),
    }))
}

/// 取平台那半:歌单列表与红心数。任一步失败都只记一笔日志、当作空 ——
/// 见 [`playlists`] 顶上那条理由。
async fn platform_playlists(
    library: &mut LibraryServiceClient<Channel>,
    account: &Account,
    netease_user_id: &str,
) -> (Vec<PlaylistDto>, i32) {
    let lists = match library
        .list_user_playlists(bangdream::as_user(
            account,
            ListUserPlaylistsRequest {
                platform: Platform::Netease as i32,
                user_id: netease_user_id.to_owned(),
                limit: 0,
                offset: 0,
            },
        ))
        .await
    {
        Ok(response) => response
            .into_inner()
            .playlists
            .into_iter()
            .map(bangdream::playlist_to_dto)
            .collect(),
        Err(status) => {
            tracing::warn!(%status, "取平台歌单失败,只给本地那半");
            Vec::new()
        }
    };

    let liked_count = match library
        .list_liked_tracks(bangdream::as_user(
            account,
            ListLikedTracksRequest {
                platform: Platform::Netease as i32,
                user_id: netease_user_id.to_owned(),
            },
        ))
        .await
    {
        Ok(response) => response
            .into_inner()
            .track_ids
            .len()
            .try_into()
            .unwrap_or(i32::MAX),
        Err(status) => {
            tracing::warn!(%status, "取红心列表失败,数目按 0 显示");
            0
        }
    };

    (lists, liked_count)
}

/// `POST /playlists` 的请求体。
#[derive(Deserialize)]
struct NameBody {
    name: String,
}

/// `POST /playlists` —— 建一个本地歌单。
async fn create_playlist(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<NameBody>,
) -> Result<Json<PlaylistDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let created =
        playlist::create(&mut conn, account.id, &body.name)
            .await
            .map_err(|err| error::map_error(&err))?;

    Ok(Json(created.to_dto()))
}

/// `PATCH /playlists/{id}` —— 给本地歌单改名。
async fn rename_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(body): Json<NameBody>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::rename(&mut conn, account.id, id, &body.name)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /playlists/{id}` —— 删掉本地歌单。
async fn delete_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::delete(&mut conn, account.id, id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /playlists/{id}/tracks` —— 本地歌单的曲目,详情由上游补全。
///
/// 与 [`liked`] 同一个套路:自家只存标识,曲目的真相在平台。
async fn playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Query(query): Query<PageQuery>,
) -> Result<Json<TracksDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let refs = playlist::tracks(&mut conn, account.id, id)
        .await
        .map_err(|err| error::map_error(&err))?;

    // 目前只有网易云一个平台。多平台之后这里要按 platform 分组各问各的,
    // 那时 GetTracks 的一次调用装不下整页 —— 留到真有第二个平台时再改。
    let ids: Vec<String> = refs
        .iter()
        .map(|track| track.track_id.clone())
        .collect();
    let page = paging::page(
        &ids,
        query.offset.unwrap_or_default(),
        query.limit.unwrap_or_default(),
    );
    if page.is_empty() {
        return Ok(Json(TracksDto { tracks: Vec::new() }));
    }

    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_tracks(bangdream::as_user(
            &account,
            GetTracksRequest {
                platform: Platform::Netease as i32,
                track_ids: page.to_vec(),
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(TracksDto {
        tracks: response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
    }))
}

/// 增删曲目的请求体。
#[derive(Deserialize)]
struct TracksBody {
    /// 曲目标识。身份是 `(平台, 平台内 id)`,所以平台不能省。
    tracks: Vec<TrackRefDto>,
}

#[derive(Deserialize)]
struct TrackRefDto {
    platform: String,
    id: String,
}

impl TracksBody {
    fn refs(&self) -> Vec<TrackRef> {
        self.tracks
            .iter()
            .map(|track| TrackRef {
                platform: track.platform.clone(),
                track_id: track.id.clone(),
            })
            .collect()
    }
}

/// `POST /playlists/{id}/tracks` —— 往本地歌单加曲目。
async fn add_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(body): Json<TracksBody>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::add_tracks(
        &mut conn,
        account.id,
        id,
        &body.refs(),
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /playlists/{id}/tracks` —— 从本地歌单移掉曲目。
async fn remove_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<i64>,
    Json(body): Json<TracksBody>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    playlist::remove_tracks(
        &mut conn,
        account.id,
        id,
        &body.refs(),
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /play/{track_id}` —— 取一条临时直链。
///
/// 每次都向上游重新要:直链带签名会过期,缓存它只会让客户端拿到放不出声的地址。
async fn play(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<Json<PlaySourceDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_play_source(bangdream::as_user(
            &account,
            GetPlaySourceRequest {
                platform: Platform::Netease as i32,
                track_id,
                level: PLAY_QUALITY as i32,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // source 缺席意味着上游认为拿到了、却没给内容 —— 当成上游失败,不静默返回空。
    let source = response.source.ok_or_else(|| {
        fail(&tonic::Status::internal("上游没有返回播放源"))
    })?;

    Ok(Json(bangdream::play_source_to_dto(source)))
}

/// `GET /lyric/{track_id}`:取一首歌的行级歌词。
///
/// 与 [`play`] 的一处刻意不同:`lyric` 缺席**不算失败**,给空行表。
/// 纯音乐与上游未收录都会走到这里,而「这首歌没有歌词」是正常状态 ——
/// 报成错误的话,客户端会把它显示成一次故障。
async fn lyric(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<Json<LyricDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_lyric(bangdream::as_user(
            &account,
            GetLyricRequest {
                platform: Platform::Netease as i32,
                track_id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(bangdream::lyric_to_dto(
        response.lyric.unwrap_or_default(),
    )))
}
