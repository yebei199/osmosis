//! 对着真实 bang-dream 跑的联机测试。
//!
//! 默认 `#[ignore]`,`cargo test` 不会碰它们 —— 它们需要外部进程,还需要一个
//! 已登录的网易云账号,不满足条件时的失败没有诊断价值。
//!
//! 跑法(沿用 `just server-test` 的惯例):
//!
//! ```bash
//! just bang-dream                       # 另一个终端:起 gRPC 服务
//! cargo test -p server -- --ignored
//! ```
//!
//! 单元测试证明的是翻译逻辑对不对;这两条证明的是**寻址与 proto 生成**对不对 ——
//! 服务名、方法名、字段号任何一处错了,单元测试都发现不了。

use tonic::transport::Channel;

use server::account::Account;
use server::bangdream::{
    self,
    proto::{
        GetAccountStatusRequest,
        GetDailyRecommendationsRequest,
        GetPlaySourceRequest, GetTracksRequest,
        ListLikedTracksRequest, Platform, QualityLevel,
        SearchArtistsRequest, SearchPlaylistsRequest,
        SearchTracksRequest, SetTrackLikedRequest,
        auth_service_client::AuthServiceClient,
        catalog_service_client::CatalogServiceClient,
        discover_service_client::DiscoverServiceClient,
        library_service_client::LibraryServiceClient,
    },
};

/// 与 `main.rs` 的默认上游地址一致。那个常量属于进程装配,不在 lib 里,
/// 这里重复一次 —— 它写错了下面两条测试立刻连不上,不会静默漂移。
const UPSTREAM: &str = "http://127.0.0.1:50051";

/// 上游按这个账号分片保存平台凭据。测试用哪个账号取决于本机
/// `data/credentials/` 下扫过码的那份 —— 用 `LIVE_USER_ID` 指定。
fn live_account() -> Account {
    Account {
        id: std::env::var("LIVE_USER_ID")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        username: "live".to_owned(),
    }
}

/// 把消息包成带用户标识的请求。少了它上游一律 INVALID_ARGUMENT。
fn req<T>(message: T) -> tonic::Request<T> {
    bangdream::as_user(&live_account(), message)
}

async fn catalog() -> CatalogServiceClient<Channel> {
    let channel = Channel::from_static(UPSTREAM)
        .connect()
        .await
        .expect("bang-dream 没起来?先跑 `just bang-dream`");
    CatalogServiceClient::new(channel)
}

/// 真实往返一次搜索:证明服务名、方法名与请求字段号都对得上。
/// 关键词用日文原名,顺带验证 UTF-8 一路没被破坏。
#[tokio::test]
#[ignore = "需要 bang-dream 在 127.0.0.1:50051 上运行"]
async fn search_returns_tracks_from_live_bangdream() {
    let response = catalog()
        .await
        .search_tracks(req(SearchTracksRequest {
            platform: Platform::Netease as i32,
            keyword: "紅蓮華".to_owned(),
            limit: 5,
            offset: 0,
        }))
        .await
        .expect("搜索请求失败")
        .into_inner();

    assert!(!response.tracks.is_empty(), "搜不到任何结果");

    let dto = bangdream::track_to_dto(
        response
            .tracks
            .into_iter()
            .next()
            .expect("已断言非空"),
    );
    assert_eq!(dto.platform, "netease");
    assert!(!dto.id.is_empty(), "歌曲没有 id");
    assert!(!dto.title.is_empty(), "歌曲没有标题");
}

/// 取一条真实直链。需要已登录的账号 —— 未登录时上游返回 Unauthenticated,
/// 此测试会失败,这正是它该报告的事。
#[tokio::test]
#[ignore = "需要 bang-dream 在 127.0.0.1:50051 上运行且网易云已登录"]
async fn play_source_returns_playable_url() {
    let mut client = catalog().await;

    let found = client
        .search_tracks(req(SearchTracksRequest {
            platform: Platform::Netease as i32,
            keyword: "紅蓮華".to_owned(),
            limit: 1,
            offset: 0,
        }))
        .await
        .expect("搜索请求失败")
        .into_inner();
    let track_id = found
        .tracks
        .into_iter()
        .next()
        .expect("搜不到任何结果")
        .id;

    let source = client
        .get_play_source(req(GetPlaySourceRequest {
            platform: Platform::Netease as i32,
            track_id,
            level: QualityLevel::High as i32,
        }))
        .await
        .expect("取播放地址失败(未登录?先跑 bang-dream 的 cmd/qrlogin)")
        .into_inner()
        .source
        .expect("上游没有返回播放源");

    let dto = bangdream::play_source_to_dto(source);
    assert!(
        dto.url.starts_with("http"),
        "直链不像个地址: {}",
        dto.url
    );
}

/// 每日推荐能取到真实曲目。
///
/// 上游一次给完整 `Track`,所以这条只证明寻址与登录态对 —— 不涉及补全。
#[tokio::test]
#[ignore = "需要 bang-dream 在 127.0.0.1:50051 上运行且网易云已登录"]
async fn daily_returns_tracks_from_live_upstream() {
    let channel = Channel::from_static(UPSTREAM)
        .connect()
        .await
        .expect("bang-dream 没起来?先跑 `just bang-dream`");

    let response = DiscoverServiceClient::new(channel)
        .get_daily_recommendations(
            GetDailyRecommendationsRequest {
                platform: Platform::Netease as i32,
            },
        )
        .await
        .expect("取每日推荐失败(未登录?)")
        .into_inner();

    assert!(!response.tracks.is_empty(), "每日推荐是空的");

    let dto = bangdream::track_to_dto(
        response
            .tracks
            .into_iter()
            .next()
            .expect("已断言非空"),
    );
    assert!(!dto.title.is_empty(), "推荐里的歌没有标题");
}

/// 红心列表走完三步:问账号 → 取标识 → 切一页 → 补全成曲目。
///
/// 这条独有的价值在**补全**那一步:前两步各自成功、拼起来却对不上(比如
/// 标识列表给的 id 与 `GetTracks` 认的不是同一种),只有跑完整条链才看得见。
#[tokio::test]
#[ignore = "需要 bang-dream 在 127.0.0.1:50051 上运行且网易云已登录"]
async fn liked_returns_hydrated_tracks() {
    let channel = Channel::from_static(UPSTREAM)
        .connect()
        .await
        .expect("bang-dream 没起来?先跑 `just bang-dream`");

    let account = AuthServiceClient::new(channel.clone())
        .get_account_status(req(GetAccountStatusRequest {
            platform: Platform::Netease as i32,
        }))
        .await
        .expect("查账号状态失败")
        .into_inner();
    assert!(
        account.logged_in,
        "没登录,先跑 `just bang-dream-login`"
    );

    let liked = LibraryServiceClient::new(channel.clone())
        .list_liked_tracks(req(ListLikedTracksRequest {
            platform: Platform::Netease as i32,
            user_id: account.user_id,
        }))
        .await
        .expect("取红心列表失败")
        .into_inner();
    assert!(!liked.track_ids.is_empty(), "一首红心都没有?");

    // 只验往返,取前几个就够 —— 973 首全取一遍是在测网易云的耐心
    let ids =
        &liked.track_ids[..5.min(liked.track_ids.len())];
    let response = CatalogServiceClient::new(channel)
        .get_tracks(req(GetTracksRequest {
            platform: Platform::Netease as i32,
            track_ids: ids.to_vec(),
        }))
        .await
        .expect("补全曲目失败")
        .into_inner();

    assert_eq!(
        response.tracks.len(),
        ids.len(),
        "给了 {} 个标识却只补全出 {} 首",
        ids.len(),
        response.tracks.len()
    );
    let dto = bangdream::track_to_dto(
        response
            .tracks
            .into_iter()
            .next()
            .expect("已断言非空"),
    );
    assert!(!dto.title.is_empty(), "补全出来的歌没有标题");
}

/// 搜歌手真实往返一次。与搜歌是同一个上游接口、不同的类型码,单元测试
/// 发现不了服务名/方法名/字段号这一类错 —— 那正是这条测试存在的理由。
#[tokio::test]
#[ignore = "需要 bang-dream 在 127.0.0.1:50051 上运行"]
async fn search_artists_returns_artists_from_live_bangdream()
 {
    let response = catalog()
        .await
        .search_artists(req(SearchArtistsRequest {
            platform: Platform::Netease as i32,
            keyword: "Beyond".to_owned(),
            limit: 5,
            offset: 0,
        }))
        .await
        .expect("搜歌手请求失败")
        .into_inner();

    assert!(!response.artists.is_empty(), "搜不到任何歌手");

    let dto = bangdream::artist_to_dto(
        response
            .artists
            .into_iter()
            .next()
            .expect("已断言非空"),
    );
    assert!(!dto.id.is_empty(), "歌手没有 id");
    assert!(!dto.name.is_empty(), "歌手没有名字");
}

/// 搜歌单真实往返一次,同上。
#[tokio::test]
#[ignore = "需要 bang-dream 在 127.0.0.1:50051 上运行"]
async fn search_playlists_returns_playlists_from_live_bangdream()
 {
    let response = catalog()
        .await
        .search_playlists(req(SearchPlaylistsRequest {
            platform: Platform::Netease as i32,
            keyword: "华语".to_owned(),
            limit: 5,
            offset: 0,
        }))
        .await
        .expect("搜歌单请求失败")
        .into_inner();

    assert!(
        !response.playlists.is_empty(),
        "搜不到任何歌单"
    );

    let dto = bangdream::playlist_to_dto(
        response
            .playlists
            .into_iter()
            .next()
            .expect("已断言非空"),
    );
    assert!(!dto.id.is_empty(), "歌单没有 id");
    assert!(!dto.name.is_empty(), "歌单没有名字");
    assert_eq!(
        dto.source,
        contract::PlaylistSource::Platform
    );
}

/// 点红心再取消,真实往返。**可逆**:先挑一首当前不在红心列表里的歌,
/// 所以中途失败也只会留下一首多出来的红心,不会抹掉你真点过的。
///
/// 这条测试的价值在于寻址与字段号 —— 写操作的 handler 本身没有逻辑,
/// 单元测试盖不到"服务名写错了"这一类错。
#[tokio::test]
#[ignore = "会向真实网易云账号写入(可逆),需已登录"]
async fn liking_a_track_is_reversible() {
    let channel = Channel::from_static(UPSTREAM)
        .connect()
        .await
        .expect("bang-dream 没起来?先跑 `just bang-dream`");
    let mut auth = AuthServiceClient::new(channel.clone());
    let mut library = LibraryServiceClient::new(channel);
    let mut catalog = catalog().await;

    let account = auth
        .get_account_status(req(GetAccountStatusRequest {
            platform: Platform::Netease as i32,
        }))
        .await
        .expect("查账号状态失败")
        .into_inner();
    assert!(account.logged_in, "需要已登录的网易云账号");

    let already: std::collections::HashSet<String> =
        library
            .list_liked_tracks(req(
                ListLikedTracksRequest {
                    platform: Platform::Netease as i32,
                    user_id: account.user_id.clone(),
                },
            ))
            .await
            .expect("取红心列表失败")
            .into_inner()
            .track_ids
            .into_iter()
            .collect();

    // 挑一首还没被红心的歌 —— 否则最后那步取消会抹掉本来就点过的
    let candidate = catalog
        .search_tracks(req(SearchTracksRequest {
            platform: Platform::Netease as i32,
            keyword: "紅蓮華".to_owned(),
            limit: 20,
            offset: 0,
        }))
        .await
        .expect("搜索失败")
        .into_inner()
        .tracks
        .into_iter()
        .map(|track| track.id)
        .find(|id| !already.contains(id))
        .expect("搜到的歌全都已经在红心里了,换个关键词");

    library
        .set_track_liked(req(SetTrackLikedRequest {
            platform: Platform::Netease as i32,
            track_id: candidate.clone(),
            liked: true,
        }))
        .await
        .expect("点红心失败");

    let after_like: Vec<String> = library
        .list_liked_tracks(req(ListLikedTracksRequest {
            platform: Platform::Netease as i32,
            user_id: account.user_id.clone(),
        }))
        .await
        .expect("取红心列表失败")
        .into_inner()
        .track_ids;

    // 先恢复原状再断言:断言失败会提前结束测试,放在后面就恢复不了了
    library
        .set_track_liked(req(SetTrackLikedRequest {
            platform: Platform::Netease as i32,
            track_id: candidate.clone(),
            liked: false,
        }))
        .await
        .expect(
            "取消红心失败 —— 这首歌现在多留在了红心列表里",
        );

    assert!(
        after_like.contains(&candidate),
        "点了红心却没进红心列表"
    );

    let after_unlike: Vec<String> = library
        .list_liked_tracks(req(ListLikedTracksRequest {
            platform: Platform::Netease as i32,
            user_id: account.user_id,
        }))
        .await
        .expect("取红心列表失败")
        .into_inner()
        .track_ids;

    assert!(
        !after_unlike.contains(&candidate),
        "取消了红心却还在列表里"
    );
}
