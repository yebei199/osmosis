//! 路由测试的共同夹具:一个真实的 Postgres,和一个进程内的假上游。
//!
//! 假上游是**真的 gRPC 服务端**,不是打桩的客户端:路由函数拿的是
//! `CatalogServiceClient<Channel>` 这种具体类型,中间没有可替换的接口 ——
//! 要让它走完一次调用,只能在本机端口上给它一个说得通的对端。server 桩
//! 因此由 `build.rs` 一并生成,见那里的说明。
//!
//! 库不回滚。`cached_tracks` 自己从池里取连接,没法把它塞进测试的事务里 ——
//! 所以每条测试用固定的账号名与 id 前缀,开跑先把上一轮的残留删掉。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use contract::TrackDto;
use sqlx::PgPool;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status};

use server::account::{Account, register};
use server::bangdream::proto::{
    Artist, GetAccountStatusRequest,
    GetAccountStatusResponse, GetPlaylistRequest,
    GetPlaylistResponse, GetTracksRequest,
    GetTracksResponse, ListUserPlaylistsRequest,
    ListUserPlaylistsResponse, Platform, Playlist,
    PlaylistTrackRef, Track,
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
use server::db;

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

/// 造一个干净的账号,并清掉上一轮留下的曲目详情。
///
/// 用固定的名字而不是随机名:随机名只会在开发库里越堆越多,而这里要的是
/// 「重复跑第二遍与第一遍看到的一样」。账号一删,它名下的成员关系跟着
/// 级联走;详情不挂账号,按 id 前缀单独删。
pub(crate) async fn fresh_account(
    pool: &PgPool,
    name: &str,
) -> Account {
    let mut conn =
        pool.acquire().await.expect("取不到数据库连接");

    sqlx::query(
        "DELETE FROM accounts WHERE lower(username) = lower($1)",
    )
    .bind(name)
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
        name,
        "correct horse",
        INVITE,
        INVITE,
    )
    .await
    .expect("注册应该成功")
}

/// 这条测试自己的曲目 id。带上测试名,几条测试并行时互不干扰,
/// 也让 [`fresh_account`] 的前缀删除只删自己那一批。
pub(crate) fn track_id(case: &str, n: usize) -> String {
    format!("{case}-{n}")
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
    /// 每一次 `GetTracks` 收到的 id 批次,按到达顺序记下来。
    ///
    /// 「只补缺的那些」和「按 `DETAIL_BATCH` 分批」这两条规矩,除了数它
    /// 没有别的办法验证:两种写法给出的曲目列表一模一样,差别只在问了几次。
    pub(crate) asked: Arc<Mutex<Vec<Vec<String>>>>,
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
    async fn get_account_status(
        &self,
        _request: Request<GetAccountStatusRequest>,
    ) -> Result<Response<GetAccountStatusResponse>, Status>
    {
        Ok(Response::new(GetAccountStatusResponse {
            logged_in: self.logged_in,
            user_id: self.user_id.clone(),
            nickname: "测试账号".to_owned(),
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
}

#[tonic::async_trait]
impl LibraryService for FakeUpstream {
    async fn list_user_playlists(
        &self,
        _request: Request<ListUserPlaylistsRequest>,
    ) -> Result<Response<ListUserPlaylistsResponse>, Status>
    {
        Ok(Response::new(ListUserPlaylistsResponse {
            playlists: self.playlists.clone(),
        }))
    }

    async fn get_playlist(
        &self,
        _request: Request<GetPlaylistRequest>,
    ) -> Result<Response<GetPlaylistResponse>, Status> {
        Ok(Response::new(self.playlist.clone()))
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

pub(crate) fn state(
    pool: PgPool,
    upstream: Upstream,
) -> AppState {
    AppState {
        upstream,
        pool,
        invite: INVITE.to_owned(),
    }
}
