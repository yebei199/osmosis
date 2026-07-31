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

use server::bangdream::{
    self,
    proto::{
        GetAccountStatusRequest,
        GetDailyRecommendationsRequest,
        GetPlaySourceRequest, GetTracksRequest,
        ListLikedTracksRequest, Platform, QualityLevel,
        SearchTracksRequest,
        auth_service_client::AuthServiceClient,
        catalog_service_client::CatalogServiceClient,
        discover_service_client::DiscoverServiceClient,
        library_service_client::LibraryServiceClient,
    },
};
use server::paging;

/// 与 `main.rs` 的默认上游地址一致。那个常量属于进程装配,不在 lib 里,
/// 这里重复一次 —— 它写错了下面两条测试立刻连不上,不会静默漂移。
const UPSTREAM: &str = "http://127.0.0.1:50051";

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
        .search_tracks(SearchTracksRequest {
            platform: Platform::Netease as i32,
            keyword: "紅蓮華".to_owned(),
            limit: 5,
            offset: 0,
        })
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
        .search_tracks(SearchTracksRequest {
            platform: Platform::Netease as i32,
            keyword: "紅蓮華".to_owned(),
            limit: 1,
            offset: 0,
        })
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
        .get_play_source(GetPlaySourceRequest {
            platform: Platform::Netease as i32,
            track_id,
            level: QualityLevel::High as i32,
        })
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
        .get_account_status(GetAccountStatusRequest {
            platform: Platform::Netease as i32,
        })
        .await
        .expect("查账号状态失败")
        .into_inner();
    assert!(
        account.logged_in,
        "没登录,先跑 `just bang-dream-login`"
    );

    let liked = LibraryServiceClient::new(channel.clone())
        .list_liked_tracks(ListLikedTracksRequest {
            platform: Platform::Netease as i32,
            user_id: account.user_id,
        })
        .await
        .expect("取红心列表失败")
        .into_inner();
    assert!(!liked.track_ids.is_empty(), "一首红心都没有?");

    let ids = paging::page(&liked.track_ids, 0, 5);
    let response = CatalogServiceClient::new(channel)
        .get_tracks(GetTracksRequest {
            platform: Platform::Netease as i32,
            track_ids: ids.to_vec(),
        })
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
