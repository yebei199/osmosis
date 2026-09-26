//! 路由测试的共同夹具:一个真实的 Postgres,和一个进程内的假上游。
//!
//! 假上游是**真的 gRPC 服务端**,不是打桩的客户端:路由函数拿的是
//! `CatalogServiceClient<Channel>` 这种具体类型,中间没有可替换的接口 ——
//! 要让它走完一次调用,只能在本机端口上给它一个说得通的对端。server 桩
//! 因此由 `build.rs` 一并生成,见那里的说明。
//!
//! 库不回滚。`cached_tracks` 自己从池里取连接,没法把它塞进测试的事务里 ——
//! 所以账号名与曲目 id 都带上本次测试进程独有的前缀([`scoped`]):开发库是
//! 整机一份,同一台机器上并行的另一份测试删不到、也覆盖不了这边的行(#136)。
//! 前缀里带着进程起跑的时刻,早于一天的由 [`sweep_stale_runs`] 清掉,
//! 开发库因此不会越堆越多。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use contract::TrackDto;
use sqlx::PgPool;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status};

use server::bangdream::proto::{
    Artist, CreateQrLoginRequest, CreateQrLoginResponse,
    GetAccountStatusRequest, GetAccountStatusResponse,
    GetPlaySourceRequest, GetPlaySourceResponse,
    GetPlaylistRequest, GetPlaylistResponse,
    GetTracksRequest, GetTracksResponse,
    ListLikedTracksRequest, ListLikedTracksResponse,
    ListUserPlaylistsRequest, ListUserPlaylistsResponse,
    LogoutRequest, LogoutResponse, Platform, PlaySource,
    Playlist, PlaylistTrackRef, QrLoginEvent,
    SetPlaylistSubscribedRequest,
    SetPlaylistSubscribedResponse, SetTrackLikedRequest,
    SetTrackLikedResponse, Track, WatchQrLoginRequest,
    auth_service_client::AuthServiceClient,
    auth_service_server::{AuthService, AuthServiceServer},
    catalog_service_client::CatalogServiceClient,
    catalog_service_server::{
        CatalogService, CatalogServiceServer,
    },
    discover_service_client::DiscoverServiceClient,
    library_service_client::LibraryServiceClient,
    library_service_server::{
        LibraryService, LibraryServiceServer,
    },
};
use server::store::account::{Account, register};
use server::store::db;

use crate::{AppState, Upstream};

/// 与 `main.rs` 的默认值一致。那个常量属于进程装配,不在 lib 里,
/// 这里重复一次 —— 它写错了下面每条测试都连不上,不会静默漂移。
const DEFAULT_DATABASE_URL: &str =
    "postgres://slint:devonly@127.0.0.1:5432/osmosis";

/// 造账号用的邀请码。测试自己既当配置方又当注册方,两边给同一个值。
const INVITE: &str = "let-me-in";

/// 网易云给红心歌单打的标记,与 `bangdream::dto` 里那个私有常量同值。
///
/// 重复一次是有意的:那边写错了,这边的假上游仍然按 5 摆歌单,于是
/// 「认不出红心歌单」会在测试里暴露,而不是两处一起错、一起过。
pub(crate) const LIKED_SPECIAL_TYPE: i32 = 5;

pub(crate) async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(
        |_| DEFAULT_DATABASE_URL.to_owned(),
    );

    db::connect(&url).await.unwrap_or_else(|err| {
        panic!(
            "连不上数据库({url}): {err}\n\
             起一个:just pg"
        )
    })
}

/// 本次测试进程的标签:`t<起跑的 Unix 秒>p<进程号>`。
///
/// 进程号保证同一时刻活着的两份测试不撞名;起跑时刻让 [`sweep_stale_runs`]
/// 认得出哪些是早已结束的进程留下的。两段都是数字,格式由那里的正则认。
static RUN: LazyLock<String> = LazyLock::new(|| {
    format!("t{}p{}", unix_secs(), std::process::id())
});

/// 别的进程留下的行,起跑早于这么久才清。远长于一次测试,
/// 所以清掉的不可能是还在跑的那一份。
const STALE_AFTER: Duration =
    Duration::from_secs(24 * 60 * 60);

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时钟早于 1970")
        .as_secs()
}

/// 这条测试在共享库里用的名字:本进程的标签加上测试名。
///
/// 账号名、曲目 id 前缀都从这里出,调用方只给测试名 —— 整机共享的名字
/// 传不进来。
pub(crate) fn scoped(case: &str) -> String {
    format!("{}-{case}", *RUN)
}

/// 清掉起跑早于 [`STALE_AFTER`] 的测试进程留下的账号、曲目详情与存档账目。
///
/// 每个进程只清一次。账号一删,名下的成员关系、队列跟着级联走;详情与账目不挂
/// 账号,按 id 前缀单独删。
async fn sweep_stale_runs(conn: &mut sqlx::PgConnection) {
    static SWEPT: AtomicBool = AtomicBool::new(false);
    if SWEPT.swap(true, Ordering::SeqCst) {
        return;
    }

    let cutoff = unix_secs() - STALE_AFTER.as_secs();
    for (table, column) in [
        ("accounts", "username"),
        ("platform_tracks", "track_id"),
        ("stored_tracks", "track_id"),
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} \
             WHERE {column} ~ '^t[0-9]+p[0-9]+-' \
               AND substring({column} from '^t([0-9]+)p')::bigint < $1"
        ))
        .bind(i64::try_from(cutoff).expect("时间戳越界"))
        .execute(&mut *conn)
        .await
        .unwrap_or_else(|err| panic!("清 {table} 的陈旧测试行失败: {err}"));
    }
}

/// 造一个干净的账号,名字是 [`scoped`] 给的那个,并清掉同名前缀的曲目详情。
///
/// 同一进程里同一个测试名只会造一次,那两条删除只是保险;真正防并行互删的
/// 是名字里的进程标签。
pub(crate) async fn fresh_account(
    pool: &PgPool,
    case: &str,
) -> Account {
    let name = scoped(case);
    let mut conn =
        pool.acquire().await.expect("取不到数据库连接");
    sweep_stale_runs(&mut conn).await;

    sqlx::query(
        "DELETE FROM accounts WHERE lower(username) = lower($1)",
    )
    .bind(&name)
    .execute(&mut *conn)
    .await
    .expect("清账号失败");

    sqlx::query(
        "DELETE FROM platform_tracks WHERE track_id LIKE $1",
    )
    .bind(format!("{name}-%"))
    .execute(&mut *conn)
    .await
    .expect("清曲目详情失败");

    register(
        &mut conn,
        &name,
        "correct horse",
        INVITE,
        INVITE,
    )
    .await
    .expect("注册应该成功")
}

/// 这条测试自己的曲目 id,前缀与 [`fresh_account`] 的账号名相同,
/// 所以那里的前缀删除只删自己那一批。
pub(crate) fn track_id(case: &str, n: usize) -> String {
    format!("{}-{n}", scoped(case))
}

/// 上游给的一首歌。
pub(crate) fn upstream_track(
    id: &str,
    title: &str,
) -> Track {
    Track {
        platform: Platform::Netease as i32,
        id: id.to_owned(),
        title: title.to_owned(),
        alias: String::new(),
        artists: vec![Artist {
            name: "某人".to_owned(),
            ..Artist::default()
        }],
        album: None,
        duration_ms: 200_000,
        cover: String::new(),
        quality: None,
        fee: 0,
    }
}

/// 同一首歌翻成契约之后该长的样子。
///
/// 手写而不是调 `track_to_dto`:拿被测代码自己的翻译当期望值,翻错了
/// 两边会一起错、一起过。
pub(crate) fn expected_dto(
    id: &str,
    title: &str,
) -> TrackDto {
    TrackDto {
        platform: "netease".to_owned(),
        id: id.to_owned(),
        title: title.to_owned(),
        alias: None,
        artists: vec!["某人".to_owned()],
        cover: None,
        duration_ms: 200_000,
    }
}

/// 假上游此刻的样子。每条测试自己摆:登没登、有哪些歌单、平台肯给哪些详情。
#[derive(Clone, Default)]
pub(crate) struct FakeUpstream {
    /// 网易云账号登没登。未登录是**状态**不是错误,上游用它回答。
    pub(crate) logged_in: bool,
    pub(crate) user_id: String,
    /// `ListUserPlaylists` 回的那一批。
    pub(crate) playlists: Vec<Playlist>,
    /// `GetPlaylist` 回的那一份。
    pub(crate) playlist: GetPlaylistResponse,
    /// 平台肯给详情的曲目,按 id 索引。
    ///
    /// 问到不在里面的 id 就**跳过**,不报错 —— 下架和无权限的歌在真实平台
    /// 上正是这个待遇,而那正是 `keep_available` 要处理的输入。
    pub(crate) details: HashMap<String, Track>,
    /// `CreateQRLogin` 回的那一张码。
    pub(crate) qr: CreateQrLoginResponse,
    /// `WatchQRLogin` 这一刻推的状态,取 `QRLoginState` 的枚举值。
    pub(crate) qr_state: i32,
    /// `Logout` 被调了几次。
    ///
    /// 解绑这条路由回的是 204,响应体里什么都没有 —— 除了数它,没有别的
    /// 办法分辨「真的转给上游了」与「什么都没干就回了 204」。
    pub(crate) logouts: Arc<Mutex<usize>>,
    /// `GetPlaySource` 回的那一条源。`None` 表示上游给不出 —— 与真实平台
    /// 「这首没有可播放的源」同义,不是一次 RPC 失败。
    pub(crate) play_source: Option<PlaySource>,
    /// `GetPlaySource` 被问了几次。直链缓存命中与否,响应里看不出来 ——
    /// 两次拿到的是同一条链接 —— 只能数上游被问了几回。
    pub(crate) play_asks: Arc<Mutex<usize>>,
    /// 每一次 `GetPlaySource` 要的档位,按到达顺序。
    pub(crate) play_levels: Arc<Mutex<Vec<i32>>>,
    /// 每一次 `GetTracks` 收到的 id 批次,按到达顺序记下来。
    ///
    /// 「只补缺的那些」和「按 `DETAIL_BATCH` 分批」这两条规矩,除了数它
    /// 没有别的办法验证:两种写法给出的曲目列表一模一样,差别只在问了几次。
    pub(crate) asked: Arc<Mutex<Vec<Vec<String>>>>,
    /// `GetPlaylist` 回答前先等这么久。
    ///
    /// 「先回库」的判据是请求**不等上游**:上游慢到一小时,等了它的实现会卡在
    /// 测试的超时上,而不是慢一点照样绿 —— 不看机器快慢。
    pub(crate) playlist_delay: std::time::Duration,
    /// `ListUserPlaylists` 回答前先等这么久。`/playlists` 的「不等上游」靠它验。
    pub(crate) lists_delay: std::time::Duration,
}

impl FakeUpstream {
    /// 一个已登录的上游,平台肯给这些曲目的详情。
    pub(crate) fn logged_in_with(
        user_id: &str,
        details: Vec<Track>,
    ) -> Self {
        Self {
            logged_in: true,
            user_id: user_id.to_owned(),
            details: details
                .into_iter()
                .map(|track| (track.id.clone(), track))
                .collect(),
            ..Self::default()
        }
    }

    /// 上游至今被要求解绑几次。
    pub(crate) fn logouts(&self) -> usize {
        *self
            .logouts
            .lock()
            .expect("记解绑次数的锁被毒化了")
    }

    /// 上游至今被要过几次播放源。
    pub(crate) fn play_asks(&self) -> usize {
        *self
            .play_asks
            .lock()
            .expect("记取源次数的锁被毒化了")
    }

    /// 至今每一次取源要的档位。
    pub(crate) fn play_levels(&self) -> Vec<i32> {
        self.play_levels
            .lock()
            .expect("记档位的锁被毒化了")
            .clone()
    }

    /// 至今为止每一批被问到的 id。
    pub(crate) fn batches(&self) -> Vec<Vec<String>> {
        self.asked
            .lock()
            .expect("记批次的锁被毒化了")
            .clone()
    }
}

#[tonic::async_trait]
impl AuthService for FakeUpstream {
    async fn create_qr_login(
        &self,
        _request: Request<CreateQrLoginRequest>,
    ) -> Result<Response<CreateQrLoginResponse>, Status>
    {
        Ok(Response::new(self.qr.clone()))
    }

    /// 只推一条就结束这条流 —— 被测的路由本来就只取第一条(它是一次轮询,
    /// 不是一条长连接)。推完不结束的话,测试要等到超时才回来。
    async fn watch_qr_login(
        &self,
        _request: Request<WatchQrLoginRequest>,
    ) -> Result<
        Response<tonic::codegen::BoxStream<QrLoginEvent>>,
        Status,
    > {
        let event = QrLoginEvent {
            state: self.qr_state,
        };

        Ok(Response::new(Box::pin(tokio_stream::once(Ok(
            event,
        )))))
    }

    async fn logout(
        &self,
        _request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        *self
            .logouts
            .lock()
            .expect("记解绑次数的锁被毒化了") += 1;

        Ok(Response::new(LogoutResponse {}))
    }

    async fn get_account_status(
        &self,
        _request: Request<GetAccountStatusRequest>,
    ) -> Result<Response<GetAccountStatusResponse>, Status>
    {
        Ok(Response::new(GetAccountStatusResponse {
            logged_in: self.logged_in,
            user_id: self.user_id.clone(),
            // 没登录时上游给的是空串,这里照着来 —— 无条件给个名字的话,
            // 「没绑却显示着昵称」这种错在测试里看不见。
            nickname: if self.logged_in {
                "测试账号".to_owned()
            } else {
                String::new()
            },
        }))
    }
}

#[tonic::async_trait]
impl CatalogService for FakeUpstream {
    async fn get_tracks(
        &self,
        request: Request<GetTracksRequest>,
    ) -> Result<Response<GetTracksResponse>, Status> {
        let ids = request.into_inner().track_ids;
        self.asked
            .lock()
            .expect("记批次的锁被毒化了")
            .push(ids.clone());

        let tracks = ids
            .iter()
            .filter_map(|id| self.details.get(id).cloned())
            .collect();

        Ok(Response::new(GetTracksResponse { tracks }))
    }

    async fn get_play_source(
        &self,
        request: Request<GetPlaySourceRequest>,
    ) -> Result<Response<GetPlaySourceResponse>, Status>
    {
        *self
            .play_asks
            .lock()
            .expect("记取源次数的锁被毒化了") += 1;
        self.play_levels
            .lock()
            .expect("记档位的锁被毒化了")
            .push(request.get_ref().level);

        Ok(Response::new(GetPlaySourceResponse {
            source: self.play_source.clone(),
        }))
    }
}

#[tonic::async_trait]
impl LibraryService for FakeUpstream {
    async fn list_user_playlists(
        &self,
        _request: Request<ListUserPlaylistsRequest>,
    ) -> Result<Response<ListUserPlaylistsResponse>, Status>
    {
        tokio::time::sleep(self.lists_delay).await;
        Ok(Response::new(ListUserPlaylistsResponse {
            playlists: self.playlists.clone(),
        }))
    }

    async fn get_playlist(
        &self,
        _request: Request<GetPlaylistRequest>,
    ) -> Result<Response<GetPlaylistResponse>, Status> {
        tokio::time::sleep(self.playlist_delay).await;
        Ok(Response::new(self.playlist.clone()))
    }

    /// 红心标识就是 `GetPlaylist` 那份成员关系:两处给同一批,不必各摆一遍。
    async fn list_liked_tracks(
        &self,
        _request: Request<ListLikedTracksRequest>,
    ) -> Result<Response<ListLikedTracksResponse>, Status>
    {
        Ok(Response::new(ListLikedTracksResponse {
            track_ids: self
                .playlist
                .track_refs
                .iter()
                .map(|track| track.id.clone())
                .collect(),
        }))
    }

    async fn set_playlist_subscribed(
        &self,
        _request: Request<SetPlaylistSubscribedRequest>,
    ) -> Result<
        Response<SetPlaylistSubscribedResponse>,
        Status,
    > {
        Ok(Response::new(
            SetPlaylistSubscribedResponse::default(),
        ))
    }

    async fn set_track_liked(
        &self,
        _request: Request<SetTrackLikedRequest>,
    ) -> Result<Response<SetTrackLikedResponse>, Status>
    {
        Ok(Response::new(SetTrackLikedResponse::default()))
    }
}

/// 一个带红心标记的歌单条目。
pub(crate) fn liked_playlist(id: &str) -> Playlist {
    Playlist {
        platform: Platform::Netease as i32,
        id: id.to_owned(),
        name: "我喜欢的音乐".to_owned(),
        special_type: LIKED_SPECIAL_TYPE,
        ..Playlist::default()
    }
}

/// 歌单里的一条成员关系。
pub(crate) fn track_ref(
    id: &str,
    added_at_ms: i64,
) -> PlaylistTrackRef {
    PlaylistTrackRef {
        id: id.to_owned(),
        added_at_ms,
    }
}

/// 起一个假上游,返回连过去的四个客户端。
///
/// 端口交给内核挑(`:0`),并行的测试因此撞不上。监听套接字在 spawn 之前
/// 就绑好了,所以客户端即使抢先连上来也只是排在 accept 队列里,不会被拒。
/// 服务端任务与测试的 runtime 同寿:`#[tokio::test]` 结束时一并回收。
pub(crate) async fn serve(fake: FakeUpstream) -> Upstream {
    let listener =
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑不上本地端口");
    let addr =
        listener.local_addr().expect("取不到本地地址");

    tokio::spawn(async move {
        Server::builder()
            .add_service(AuthServiceServer::new(
                fake.clone(),
            ))
            .add_service(CatalogServiceServer::new(
                fake.clone(),
            ))
            .add_service(LibraryServiceServer::new(fake))
            .serve_with_incoming(TcpListenerStream::new(
                listener,
            ))
            .await
    });

    upstream_at(&format!("http://{addr}"))
}

/// 指向某个地址的四个客户端。连接是惰性的,与 `main.rs` 一致 ——
/// 地址上没有东西时,失败发生在第一次调用而不是这里。
pub(crate) fn upstream_at(url: &str) -> Upstream {
    let channel = Channel::from_shared(url.to_owned())
        .expect("上游地址不是合法 URI")
        .connect_lazy();
    let channel = server::bangdream::Timed(channel);

    Upstream {
        catalog: CatalogServiceClient::new(channel.clone()),
        library: LibraryServiceClient::new(channel.clone()),
        discover: DiscoverServiceClient::new(
            channel.clone(),
        ),
        auth: AuthServiceClient::new(channel),
    }
}

/// 一个连不上的上游。
///
/// 端口 1 是特权端口,本机上不会有人监听,连接立刻被拒 —— 不必等超时,
/// 也不必先绑一个端口再放开(那中间有人抢进来就成了偶发失败)。
pub(crate) fn unreachable_upstream() -> Upstream {
    upstream_at("http://127.0.0.1:1")
}

/// 同一份 state 换一个上游:库与「缓存有多新」的记录都沿用。
///
/// 平台那边变了、或者卡住了,在测试里就是换一个假上游 —— 假上游起来之后
/// 就不能再改,而被测的正是「同一个进程前后两次请求」。
pub(crate) fn with_upstream(
    state: &AppState,
    upstream: Upstream,
) -> AppState {
    AppState {
        upstream,
        ..state.clone()
    }
}

pub(crate) fn state(
    pool: PgPool,
    upstream: Upstream,
) -> AppState {
    AppState {
        upstream,
        pool,
        invite: INVITE.to_owned(),
        roster: server::syncplay::signaling::SharedRoster::default(),
        origins: server::syncplay::signaling::AllowedOrigins::default(
        ),
        policies:
            server::gate::ratelimit::Policies::tuned(),
        playlists: Default::default(),
        links: Default::default(),
        apk_releases: crate::routes::apk::DEFAULT_RELEASES_BASE
            .to_owned(),
        archive: None,
    }
}

/// 内存里的对象存储,替掉 S3 —— 路由测试不必起 RustFS。
///
/// 签出来的「链接」是 `memory://键`:客户端不会真去取它,测试只看交出的是不是它。
#[derive(Default)]
pub(crate) struct MemoryObjects {
    objects: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryObjects {
    /// 这个键下存的字节。
    pub(crate) fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.objects
            .lock()
            .expect("对象表的锁被毒化了")
            .get(key)
            .cloned()
    }

    /// 从背后删掉一个对象,模拟桶里的东西丢了。
    pub(crate) fn lose(&self, key: &str) {
        self.objects
            .lock()
            .expect("对象表的锁被毒化了")
            .remove(key);
    }
}

impl server::objects::Objects for MemoryObjects {
    fn put(
        &self,
        key: &str,
        body: server::objects::ObjectBody,
        length: u64,
        _content_type: &'static str,
    ) -> futures_util::future::BoxFuture<
        '_,
        server::objects::ObjectResult<()>,
    > {
        use futures_util::TryStreamExt;
        let key = key.to_owned();
        Box::pin(async move {
            let bytes: Vec<u8> = body
                .map_ok(|chunk| chunk.to_vec())
                .try_concat()
                .await
                .map_err(|err| err.to_string())?;
            // 与 S3 同一条规矩:长度对不上就不存
            if bytes.len() as u64 != length {
                return Err(format!(
                    "说好 {length} 字节,来了 {}",
                    bytes.len()
                ));
            }
            self.objects
                .lock()
                .expect("对象表的锁被毒化了")
                .insert(key, bytes);
            Ok(())
        })
    }

    fn exists(
        &self,
        key: &str,
    ) -> futures_util::future::BoxFuture<
        '_,
        server::objects::ObjectResult<bool>,
    > {
        let found = self.get(key).is_some();
        Box::pin(async move { Ok(found) })
    }

    fn delete(
        &self,
        key: &str,
    ) -> futures_util::future::BoxFuture<
        '_,
        server::objects::ObjectResult<()>,
    > {
        self.lose(key);
        Box::pin(async { Ok(()) })
    }

    fn presign_get(&self, key: &str) -> String {
        format!("memory://{key}")
    }
}
