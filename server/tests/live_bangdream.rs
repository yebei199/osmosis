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
        GetPlaySourceRequest, Platform, QualityLevel,
        SearchTracksRequest,
        catalog_service_client::CatalogServiceClient,
    },
};

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
