//! bang-dream 聚合层的 gRPC 客户端,以及它的领域模型到 [`contract`] 的翻译。
//!
//! 这里是本服务唯一认识 gRPC 的地方 —— 往上只有 [`contract`] 里的 DTO。
//! 客户端因此不必知道 bang-dream 的存在,也不必为上游 proto 的演化重新编译。
//!
//! 翻译刻意**裁剪**:上游的 `Track` 有音质规格、付费等级等等,这里只留客户端
//! 此刻用得上的字段。加字段是兼容变更,用到时再加。

mod dto;

mod lyric_split;

mod track_refs;

pub use dto::{
    account_status_to_dto, artist_to_dto,
    liked_playlist_id, lyric_to_dto,
    platform_playlists_to_dto, play_source_to_dto,
    playlist_to_dto, qr_login_to_dto, qr_state_name,
    track_to_dto,
};
pub use track_refs::{keep_available, refs_missing_from};

use crate::store::account::Account;

/// 上游那条 gRPC 通道,包上了每次调用的计时(见 [`Timed`])。
pub type UpstreamChannel = Timed<tonic::transport::Channel>;

/// 给一条 gRPC 通道包上计时:每次调用在收到响应头时打一行
/// `upstream method=<gRPC 方法> ms=<毫秒>`(#121)。
///
/// 一元调用的响应头要等上游处理完才发,所以这个数就是「bang-dream 连同它
/// 回源网易云花了多久」。这一行打在调用方当时的 span 里 —— 路由中间件给每个
/// 请求开了一个带 id 的 span,上游耗时于是能按 id 归到那条路由上。
#[derive(Clone, Debug)]
pub struct Timed<S>(pub S);

impl<S, B, R>
    tower::Service<tonic::codegen::http::Request<B>>
    for Timed<S>
where
    S: tower::Service<
            tonic::codegen::http::Request<B>,
            Response = tonic::codegen::http::Response<R>,
        >,
    S::Future: Send + 'static,
    S::Error: std::fmt::Display,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = tonic::codegen::BoxFuture<
        Self::Response,
        Self::Error,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(
        &mut self,
        request: tonic::codegen::http::Request<B>,
    ) -> Self::Future {
        let method = request.uri().path().to_owned();
        let started = std::time::Instant::now();
        let future = self.0.call(request);
        Box::pin(async move {
            let result = future.await;
            let ms = started.elapsed().as_millis();
            match &result {
                Ok(_) => {
                    tracing::info!(%method, ms, "upstream")
                }
                Err(error) => {
                    tracing::warn!(%method, ms, %error, "upstream")
                }
            }
            result
        })
    }
}

/// 由 `build.rs` 从 `third_party/bang-dream/proto` 生成。
pub mod proto {
    tonic::include_proto!("bangdream.music.v1");
}

/// bang-dream 认这个 metadata 键来选该用哪个账号的网易云凭据。
///
/// 键名与那侧的 `internal/rpc/userid.go` 手工对齐 —— proto 里没有它,
/// 因为「以谁的身份问」不是领域请求的一部分(见那个仓库的 `docs/adr/0009`)。
const USER_ID_KEY: &str = "x-user-id";

/// 把一条请求包成带用户标识的 gRPC 请求。
///
/// 上游没有默认用户:不带这个头的调用一律 `INVALID_ARGUMENT`。所以每一次上游调用
/// 都要经过这里 —— 漏一处的现象是那条路由整个不可用,不会静默串号。
pub fn as_user<T>(
    account: &Account,
    message: T,
) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    // 用户标识是 accounts.id 的十进制串,永远是合法的 ASCII metadata 值
    let value = account.upstream_user_id().parse().expect(
        "用户标识是十进制数字,必然是合法的 metadata 值",
    );

    request.metadata_mut().insert(USER_ID_KEY, value);

    request
}

/// 上游平台枚举翻成契约里的字符串。
///
/// 用字符串而非数字:契约要能被人读懂,也要在加平台时不依赖枚举序号的稳定性。
///
/// 缓存也按这个值存(见 `cache.rs`)—— 另写一份的话,prost 生成的
/// `as_str_name()` 给的是 `PLATFORM_NETEASE`,与这里的 `netease` 对不上,
/// 而那是运行期才炸的外键错误,编译器一声不吭。
pub fn platform_name(raw: i32) -> String {
    match proto::Platform::try_from(raw) {
        Ok(proto::Platform::Netease) => "netease",
        _ => "unknown",
    }
    .to_owned()
}

/// 空串翻成 `None`。
///
/// protobuf 的 `string` 没有"缺席"这个状态,平台没给就是空串。契约区分二者:
/// `None` 是"平台没给",`Some("")` 会被客户端当成一个真实存在的空值。
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests;
