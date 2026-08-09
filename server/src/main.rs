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
    ArtistSearchDto, HealthDto, LoginDto, LyricDto,
    PROTOCOL_VERSION, PlaySourceDto, PlayedDto,
    PlaylistDto, PlaylistSearchDto, PlaylistsDto,
    RegisterDto, SearchDto, SessionDto, TrackDto,
    TrackIdsDto, TracksDto,
};
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::HashSet;
use tonic::transport::Channel;
use tower_http::cors::CorsLayer;

use server::account::{self, Account};
use server::bangdream::{
    self,
    proto::{
        GetAccountStatusRequest, GetArtistRequest,
        GetDailyRecommendationsRequest, GetLyricRequest,
        GetPlaySourceRequest, GetPlaylistRequest,
        GetPlaylistResponse, GetTracksRequest,
        ListLikedTracksRequest, ListUserPlaylistsRequest,
        Platform, QualityLevel, SearchArtistsRequest,
        SearchPlaylistsRequest, SearchTracksRequest,
        SetPlaylistSubscribedRequest, SetTrackLikedRequest,
        auth_service_client::AuthServiceClient,
        catalog_service_client::CatalogServiceClient,
        discover_service_client::DiscoverServiceClient,
        library_service_client::LibraryServiceClient,
    },
};
use server::error::Failure;
use server::history;
use server::playlist::{self, TrackRef};
use server::signaling::{self, SharedRoster};
use server::{cache, db, error};

/// 默认监听地址。
///
/// 绑 `127.0.0.1` 而非 `0.0.0.0`:手机通过 `adb reverse tcp:3000 tcp:3000`
/// 把自己的 `127.0.0.1:3000` 转发到这里,不需要服务端暴露在局域网上。
const DEFAULT_BIND: &str = "127.0.0.1:3000";

/// bang-dream 聚合层的默认地址,与它的 `cmd/bang-dream` 默认监听一致。
const DEFAULT_UPSTREAM: &str = "http://127.0.0.1:50051";

/// 搜索默认返回条数。
const DEFAULT_SEARCH_LIMIT: i32 = 30;

/// 一次向上游要多少首曲目详情。
///
/// 973 首的歌单一次要不完 —— 上游把这些 id 拼进一个请求体发给平台,而平台对
/// 请求大小有自己的想法。分批只在**冷启动**发生:详情缓存下来之后,常态是
/// 一批都不用要。
const DETAIL_BATCH: usize = 200;

/// 取播放地址时请求的音质档位。
///
// ponytail: 先写死。做到音质选择时再提成查询参数 —— 现在没有任何界面能选它。
const PLAY_QUALITY: QualityLevel = QualityLevel::High;

/// 「最近播放」默认给多少首。
///
// ponytail: 一屏够看就行,客户端要更多可以自己传 limit。
const DEFAULT_RECENT_LIMIT: i64 = 50;

/// 数据库连接串的默认值,与 `just pg` 起的容器一致。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

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

/// 三条搜索路由共用的查询参数。
#[derive(Deserialize)]
struct SearchQuery {
    /// 关键词。
    q: String,
    /// 每页条数,不给按 [`DEFAULT_SEARCH_LIMIT`]。
    limit: Option<i32>,
    /// 偏移量,不给从 0 开始。翻页由客户端自行推进。
    offset: Option<i32>,
}

/// `GET /search/tracks?q=紅蓮華` —— 搜歌。
async fn search_tracks(
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

/// `GET /search/artists?q=beyond` —— 搜歌手。
async fn search_artists(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<SearchQuery>,
) -> Result<Json<ArtistSearchDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .search_artists(bangdream::as_user(
            &account,
            SearchArtistsRequest {
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

    Ok(Json(ArtistSearchDto {
        artists: response
            .artists
            .into_iter()
            .map(bangdream::artist_to_dto)
            .collect(),
        has_more: response.has_more,
    }))
}

/// `GET /artists/{id}/tracks` —— 某个歌手的热门曲目。
///
/// 搜索结果里的歌手点下去要能听到东西,否则那一页只是一串名字。
///
/// 上游一次给完整曲目,不像歌单那样只给标识 —— 因此不经过缓存:没有要补的详情,
/// 而这批歌是**平台此刻认为的热门**,存下来只会让它停在过去某一天。
async fn artist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Result<Json<TracksDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_artist(bangdream::as_user(
            &account,
            GetArtistRequest {
                platform: Platform::Netease as i32,
                artist_id: id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    Ok(Json(TracksDto {
        tracks: response
            .hot_tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect(),
    }))
}

/// `GET /search/playlists?q=华语` —— 搜歌单。
///
/// 只搜平台的。本地歌单数量小、已经在客户端手上,过滤是界面的事,
/// 为它多跑一趟服务端没有意义。
async fn search_playlists(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<SearchQuery>,
) -> Result<Json<PlaylistSearchDto>, Failure> {
    let mut catalog = state.upstream.catalog;
    let response = catalog
        .search_playlists(bangdream::as_user(
            &account,
            SearchPlaylistsRequest {
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

    Ok(Json(PlaylistSearchDto {
        // 搜索结果里不会有红心歌单(那是账号自己的),照直翻就行
        playlists: response
            .playlists
            .into_iter()
            .map(bangdream::playlist_to_dto)
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

/// `GET /recent` 的查询参数。
///
/// 只剩 limit 一个:歌单类的路由都不再切页了,它们要的是完整的一批。
/// 最近播放不同 —— 那是一条越来越长的流水,「最近多少条」是它的固有参数。
#[derive(Deserialize)]
struct PageQuery {
    limit: Option<usize>,
}

/// `GET /liked` —— 我喜欢的音乐,全量。
///
/// 三步:先问上游当前账号是谁,再拿它**全量**的红心标识列表,把缺详情的那些补齐。
/// 上游只给标识是刻意的(它的 `docs/adr/0003`):平台返回的曲目列表会被截断,
/// 标识列表不会。
///
/// user_id 不做缓存:重新扫码登录会换一个账号,缓存住的话红心列表会静默停在旧账号上。
/// 这是一次同机 gRPC,便宜得没必要省。
async fn liked_ids(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TrackIdsDto>, Failure> {
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

    // 没绑网易云是**状态**不是错误:那就是「一首红心都没有」,
    // 界面据此把所有心画成空的,而不是整页失败。
    if !netease_account.logged_in {
        return Ok(Json(TrackIdsDto {
            track_ids: Vec::new(),
        }));
    }

    let found = library
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

    Ok(Json(TrackIdsDto {
        track_ids: found.track_ids,
    }))
}

/// 把一个平台歌单的曲目备齐,并按平台给的次序读出来。
///
/// 只向平台要**缺详情**的那些:歌单的成员关系天天变(点一次红心就变一次),
/// 而曲目详情几乎不变,且跨歌单共用 —— 收藏的歌单里的歌大半已经在红心里了。
/// 每次都全量重取的话,这个缓存等于没有(见 `docs/adr/0018`)。
///
/// 平台不肯给详情的 id(下架、无权限)会被剔出成员关系:留着它只会在读回时
/// 的 JOIN 里消失,那时歌单少一首而没有任何人报错。
async fn cached_tracks(
    state: &AppState,
    account: &Account,
    playlist_id: &str,
    refs: &[cache::TrackRef],
    detail_tracks: &[TrackDto],
) -> Result<Vec<TrackDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let platform = netease_name();

    // 歌单详情随手带回来的那一批先入库,它们不必再问平台要一遍。
    // 平台把这一批截断时,只有差额才走补拉。
    cache::put_details(&mut conn, detail_tracks)
        .await
        .map_err(|err| error::map_error(&err))?;
    let missing =
        bangdream::refs_missing_from(detail_tracks, refs);

    let unavailable =
        fill_details(state, account, &mut conn, &missing)
            .await?;
    let known: Vec<cache::TrackRef> = refs
        .iter()
        .filter(|track| !unavailable.contains(&track.id))
        .cloned()
        .collect();

    cache::set_membership(
        &mut conn,
        account.id,
        playlist_id,
        &platform,
        &known,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    cache::tracks_of(&mut conn, account.id, playlist_id)
        .await
        .map_err(|err| error::map_error(&err))
}

/// 把这些 id 里还缺的详情向平台要回来存下,返回平台**仍然给不出**的那些。
///
/// 分成两步问「谁还缺」是有意的:第一次问的是「要不要发请求」,第二次问的是
/// 「发完了还差谁」。合成一次的话,平台跳过的那些(下架、无权限)与从没问过的
/// 那些混在一起,分不出来。
async fn fill_details(
    state: &AppState,
    account: &Account,
    conn: &mut sqlx::PgConnection,
    ids: &[String],
) -> Result<HashSet<String>, Failure> {
    let platform = netease_name();

    let missing =
        cache::missing_details(conn, &platform, ids)
            .await
            .map_err(|err| error::map_error(&err))?;

    let mut catalog = state.upstream.catalog.clone();
    for chunk in missing.chunks(DETAIL_BATCH) {
        let response = catalog
            .get_tracks(bangdream::as_user(
                account,
                GetTracksRequest {
                    platform: Platform::Netease as i32,
                    track_ids: chunk.to_vec(),
                },
            ))
            .await
            .map_err(|status| fail(&status))?
            .into_inner();

        let tracks: Vec<TrackDto> = response
            .tracks
            .into_iter()
            .map(bangdream::track_to_dto)
            .collect();

        cache::put_details(conn, &tracks)
            .await
            .map_err(|err| error::map_error(&err))?;
    }

    Ok(cache::missing_details(conn, &platform, ids)
        .await
        .map_err(|err| error::map_error(&err))?
        .into_iter()
        .collect())
}

/// 缓存里代表网易云的那个字符串。
///
/// 走 `track_to_dto` 用的同一个函数 —— 另写一份的话,prost 生成的
/// `as_str_name()` 给的是 `PLATFORM_NETEASE`,与存进去的 `netease` 对不上,
/// 而那是运行期才炸的外键错误,编译器一声不吭。
fn netease_name() -> String {
    bangdream::platform_name(Platform::Netease as i32)
}

async fn liked(
    State(state): State<AppState>,
    account: Account,
) -> Result<Json<TracksDto>, Failure> {
    let mut auth = state.upstream.auth.clone();
    let mut library = state.upstream.library.clone();

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

    // 走红心**歌单**而不是 /liked/ids 那条路:红心接口返回的是裸数字数组,
    // 结构上挂不住加入时间,而次序要按加入时间倒排(见 `docs/adr/0021`)。
    let liked_id = liked_playlist_id(
        &mut library,
        &account,
        &netease_account.user_id,
    )
    .await?;

    let detail = library
        .get_playlist(bangdream::as_user(
            &account,
            GetPlaylistRequest {
                platform: Platform::Netease as i32,
                playlist_id: liked_id,
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    // 不分页:红心是一整批,不是搜索结果。973 首里只看得到 50 首的话,
    // 剩下的 923 首没有任何入口 —— 界面上没有翻页,也不该有。
    let tracks = cached_tracks(
        &state,
        &account,
        cache::LIKED_PLAYLIST_ID,
        &track_refs_of(&detail),
        &[],
    )
    .await?;

    Ok(Json(TracksDto { tracks }))
}

/// 找出这个账号的红心歌单在平台上的 id。
///
/// 平台把红心也算作一个用户歌单,靠 `special_type` 认;上游只搬运这个值,
/// 判定归这边(见 `docs/adr/0022`)。找不到是**错误**而不是空列表 ——
/// 每个账号都有这个歌单,找不到说明上游给的列表不完整,那时回空会被读成
/// 「一首喜欢的都没有」。
async fn liked_playlist_id(
    library: &mut LibraryServiceClient<Channel>,
    account: &Account,
    netease_user_id: &str,
) -> Result<String, Failure> {
    let lists = library
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
        .map_err(|status| fail(&status))?
        .into_inner();

    bangdream::liked_playlist_id(&lists.playlists)
        .ok_or_else(|| {
            fail(&tonic::Status::not_found(
                "netease: 歌单列表里没有红心歌单",
            ))
        })
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
        Ok(response) => response.into_inner().playlists,
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

    (
        bangdream::platform_playlists_to_dto(lists),
        liked_count,
    )
}

/// `GET /playlists/platform/{id}/tracks` —— 平台歌单的曲目。
///
/// 上游只给全量标识不给曲目:平台返回的曲目列表会被截断,标识列表不会
/// (见 bang-dream 的 `docs/adr/0003`)。详情因此在这一层备齐,与 [`liked`] 同一个套路。
async fn platform_playlist_tracks(
    State(state): State<AppState>,
    account: Account,
    Path(id): Path<String>,
) -> Result<Json<TracksDto>, Failure> {
    let mut library = state.upstream.library.clone();
    let detail = library
        .get_playlist(bangdream::as_user(
            &account,
            GetPlaylistRequest {
                platform: Platform::Netease as i32,
                playlist_id: id.clone(),
            },
        ))
        .await
        .map_err(|status| fail(&status))?
        .into_inner();

    let tracks = cached_tracks(
        &state,
        &account,
        &id,
        &track_refs_of(&detail),
        // 上游此刻只回标识,不回曲目详情(见 bang-dream 的 PlaylistDetail
        // 函数文档)。空切片意味着全部走补拉,也就是改动之前的行为。
        // 等那侧把截断过的 tracks 一并带上来,这里换成它就有了快路径 ——
        // 判据在 bangdream::refs_missing_from,已经有测试钉住。
        &[],
    )
    .await?;

    Ok(Json(TracksDto { tracks }))
}

/// 歌单详情里的成员关系。
fn track_refs_of(
    detail: &GetPlaylistResponse,
) -> Vec<cache::TrackRef> {
    detail
        .track_refs
        .iter()
        .map(|track| {
            cache::TrackRef::new(
                &track.id,
                Some(track.added_at_ms),
            )
        })
        .collect()
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
) -> Result<Json<TracksDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let refs = playlist::tracks(&mut conn, account.id, id)
        .await
        .map_err(|err| error::map_error(&err))?;

    // 目前只有网易云一个平台。多平台之后这里要按 platform 分组各问各的 ——
    // 留到真有第二个平台时再改。
    let ids: Vec<String> = refs
        .iter()
        .map(|track| track.track_id.clone())
        .collect();

    // 只借详情那一半:本地歌单的成员关系真相在自家表里,不进缓存。
    // 进了的话,它的整数 id 会和平台歌单的字符串 id 撞在同一列上。
    fill_details(&state, &account, &mut conn, &ids).await?;

    let tracks =
        cache::details_of(&mut conn, &netease_name(), &ids)
            .await
            .map_err(|err| error::map_error(&err))?;

    Ok(Json(TracksDto { tracks }))
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

/// `PUT /liked/{track_id}` —— 给一首歌点红心。
async fn like_track(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_liked(&state, &account, track_id, true).await
}

/// `DELETE /liked/{track_id}` —— 取消红心。
async fn unlike_track(
    State(state): State<AppState>,
    account: Account,
    Path(track_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_liked(&state, &account, track_id, false).await
}

/// 红心的开与关只差一个布尔值,两条路由因此共用这一段。
///
/// 「我喜欢的」就是平台的红心列表,不建本地副本(见 `docs/adr/0016`),
/// 所以这里只转发,自家库一个字都不写。
async fn set_liked(
    state: &AppState,
    account: &Account,
    track_id: String,
    liked: bool,
) -> Result<StatusCode, Failure> {
    let mut library = state.upstream.library.clone();

    library
        .set_track_liked(bangdream::as_user(
            account,
            SetTrackLikedRequest {
                platform: Platform::Netease as i32,
                track_id,
                liked,
            },
        ))
        .await
        .map_err(|status| fail(&status))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /subscriptions/playlists/{playlist_id}` —— 收藏一个平台歌单。
async fn subscribe_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(playlist_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_subscribed(&state, &account, playlist_id, true)
        .await
}

/// `DELETE /subscriptions/playlists/{playlist_id}` —— 取消收藏。
async fn unsubscribe_playlist(
    State(state): State<AppState>,
    account: Account,
    Path(playlist_id): Path<String>,
) -> Result<StatusCode, Failure> {
    set_subscribed(&state, &account, playlist_id, false)
        .await
}

/// 收藏的开与关同样只差一个布尔值。
///
/// 只对**平台**歌单有意义:本地歌单是自己建的,没有"收藏"这回事,
/// 它的对应操作是删除。
async fn set_subscribed(
    state: &AppState,
    account: &Account,
    playlist_id: String,
    subscribed: bool,
) -> Result<StatusCode, Failure> {
    let mut library = state.upstream.library.clone();

    library
        .set_playlist_subscribed(bangdream::as_user(
            account,
            SetPlaylistSubscribedRequest {
                platform: Platform::Netease as i32,
                playlist_id,
                subscribed,
            },
        ))
        .await
        .map_err(|status| fail(&status))?;

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

/// `POST /played` —— 报告一次起播。
///
/// 客户端在**声音真的出来之后**才发,不是按下播放键就发:取直链可能失败,
/// 那时并没有发生一次播放。
async fn record_play(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<PlayedDto>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    history::record(
        &mut conn,
        account.id,
        &TrackRef {
            platform: body.platform,
            track_id: body.track_id,
        },
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /recent` —— 最近播放,曲目详情由上游补全。
///
/// 与 [`liked`]、[`playlist_tracks`] 同一个套路:自家只存标识。
async fn recent(
    State(state): State<AppState>,
    account: Account,
    Query(query): Query<PageQuery>,
) -> Result<Json<TracksDto>, Failure> {
    // 大得离谱的 limit 退回默认值,而不是报错:那是客户端的笔误,不是攻击
    let limit = query
        .limit
        .and_then(|limit| i64::try_from(limit).ok())
        .unwrap_or(DEFAULT_RECENT_LIMIT);

    let mut conn = conn(&state.pool).await?;
    let refs =
        history::recent(&mut conn, account.id, limit)
            .await
            .map_err(|err| error::map_error(&err))?;

    if refs.is_empty() {
        return Ok(Json(TracksDto { tracks: Vec::new() }));
    }

    // 目前只有网易云一个平台,与 playlist_tracks 同一处待办
    let ids: Vec<String> = refs
        .into_iter()
        .map(|track| track.track_id)
        .collect();

    let mut catalog = state.upstream.catalog;
    let response = catalog
        .get_tracks(bangdream::as_user(
            &account,
            GetTracksRequest {
                platform: Platform::Netease as i32,
                track_ids: ids,
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
