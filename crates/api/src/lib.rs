//! API 客户端:把"取一份服务端健康状态"这样的意图,翻译成一次具体的网络往返。
//!
//! 这里是各端运行时能力差异(有无线程、能否阻塞)被吸收的地方,差异到此为止,
//! 不再向上传播。对外暴露的 `async fn` 在 native 与 wasm 上**签名完全相同**;
//! `Send` 约束只存在于本 crate 内部的 `platform` 模块里。见 `docs/adr/0002`。

use contract::{
    ArtistSearchDto, ErrorDto, HealthDto, LoginDto,
    LyricDto, PROTOCOL_VERSION, PlaySourceDto, PlayedDto,
    PlaylistDto, PlaylistSearchDto, PlaylistsDto,
    RegisterDto, SearchDto, SessionDto, TracksDto,
};
use serde::Serialize;

/// 服务端地址。可在编译期用 `SLINT_STUDY_API_BASE` 覆盖。
///
/// 默认指向 `127.0.0.1` —— Android 上这是**手机自己**的回环地址,需要
/// `adb reverse tcp:3000 tcp:3000` 把它转发到开发机(见 `just adb-reverse`)。
pub fn base_url() -> &'static str {
    option_env!("SLINT_STUDY_API_BASE")
        .unwrap_or("http://127.0.0.1:3000")
}

/// 一次请求可能的失败方式。
///
/// 这些都不是线上格式,因此不属于 `contract`:它们描述的是"没能完成一次往返",
/// 而不是"服务端说了什么"。
#[derive(Debug)]
pub enum ApiError {
    /// 连不上、超时、非 2xx —— 请求没能走完。
    Transport(String),
    /// 走完了,但响应体不是我们认识的形状。
    Decode(String),
    /// 双方说的不是同一个版本的协议。
    VersionMismatch { expected: u32, actual: u32 },
    /// 服务端明确拒绝了,并说明了原因。
    ///
    /// 与 [`Self::Transport`] 的区别是**有没有得到答复**:这一个是服务端想清楚了
    /// 才说的不,调用方可以按 `code` 分支;那一个是话没传到。
    ///
    /// 不带 HTTP 状态码:契约规定客户端按 `code` 分支而不是按状态码
    /// (见 `contract::ErrorDto`),带上它只会诱使人去用错的那个。
    Server { code: String, message: String },
}

impl core::fmt::Display for ApiError {
    fn fmt(
        &self,
        f: &mut core::fmt::Formatter<'_>,
    ) -> core::fmt::Result {
        match self {
            Self::Transport(message) => {
                write!(f, "网络错误: {message}")
            }
            Self::Decode(message) => {
                write!(f, "响应格式错误: {message}")
            }
            Self::Server { message, .. } => {
                write!(f, "{message}")
            }
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "协议版本不匹配: 本机 v{expected},服务端 v{actual}"
                )
            }
        }
    }
}

impl core::error::Error for ApiError {}

/// `GET /health`。
///
/// 校验协议版本是 `contract` 存在的意义所在:服务端换了线上格式而客户端没跟上时,
/// 这里立刻报错,而不是让某个字段静默地变成默认值。
pub async fn health() -> Result<HealthDto, ApiError> {
    let dto: HealthDto = platform::get_json(format!(
        "{}/health",
        base_url()
    ))
    .await?;

    check_version(dto)
}

/// 版本校验本身。从 [`health`] 里抽出来,好让它离开网络单独被测 ——
/// `base_url()` 是编译期常量,同一进程内无法把请求指向一个版本不同的假服务端。
///
/// 这条分支在本仓库里是**可达的**:手机上装着的旧 APK 焊死了它编译那一刻的
/// [`PROTOCOL_VERSION`],而开发机上的 server 每次都从当前源码重新编译。
fn check_version(
    dto: HealthDto,
) -> Result<HealthDto, ApiError> {
    if dto.protocol_version != PROTOCOL_VERSION {
        return Err(ApiError::VersionMismatch {
            expected: PROTOCOL_VERSION,
            actual: dto.protocol_version,
        });
    }
    Ok(dto)
}

/// `GET /search/tracks?q=…`。
pub async fn search_tracks(
    keyword: &str,
) -> Result<SearchDto, ApiError> {
    platform::get_json(search_url("tracks", keyword)).await
}

/// `GET /search/artists?q=…`。
pub async fn search_artists(
    keyword: &str,
) -> Result<ArtistSearchDto, ApiError> {
    platform::get_json(search_url("artists", keyword)).await
}

/// `GET /search/playlists?q=…`。
///
/// 只搜平台的歌单。本地歌单已经在手上,过滤是界面的事。
pub async fn search_playlists(
    keyword: &str,
) -> Result<PlaylistSearchDto, ApiError> {
    platform::get_json(search_url("playlists", keyword))
        .await
}

/// `GET /play/{track_id}`。
///
/// 拿到的是一条**临时**直链,带签名会过期。别缓存 —— 过期后服务端返回的
/// 是一个 HTML 错误页,解码那头会报"这不是音频",症状离病因很远。
pub async fn play_source(
    track_id: &str,
) -> Result<PlaySourceDto, ApiError> {
    platform::get_json(play_url(track_id)).await
}

/// `GET /daily` —— 今日推荐。
pub async fn daily() -> Result<TracksDto, ApiError> {
    platform::get_json(format!("{}/daily", base_url()))
        .await
}

/// `GET /liked` —— 我喜欢的音乐,取第一页。
///
/// 服务端不带 limit 时用它自己的默认页大小,客户端不必知道那个数字。
pub async fn liked() -> Result<TracksDto, ApiError> {
    platform::get_json(format!("{}/liked", base_url()))
        .await
}

/// 拼搜索地址,关键词按 URL 查询串规则转义。
///
/// 抽出来单独可测:关键词直接插进 `format!` 的话,一个 `&` 就会把查询串截成
/// 两个参数,服务端只看到半截关键词 —— 而这既不会报错,也不会有测试失败。
fn search_url(kind: &str, keyword: &str) -> String {
    format!(
        "{}/search/{kind}?q={}",
        base_url(),
        encode_component(keyword)
    )
}

/// 拼播放地址。id 是路径的一段,同样要转义。
fn play_url(track_id: &str) -> String {
    format!(
        "{}/play/{}",
        base_url(),
        encode_component(track_id)
    )
}

/// `/lyric/{track_id}` 的完整地址。id 同样要转义(理由见 [`play_url`])。
fn lyric_url(track_id: &str) -> String {
    format!(
        "{}/lyric/{}",
        base_url(),
        encode_component(track_id)
    )
}

/// 百分号编码一个 URL 组件。
///
/// 手写而不是引 `percent-encoding`:规则就是"非 unreserved 字符逐字节转义",
/// 一个 crate 换不来更少的代码。unreserved 集合见 RFC 3986 §2.3。
fn encode_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => out.push(*byte as char),
            other => {
                out.push_str(&format!("%{other:02X}"));
            }
        }
    }
    out
}

/// `GET /lyric/{track_id}`。
///
/// 没有歌词(纯音乐、上游未收录)时给**空行表**而不是错误 —— 这条语义从
/// 服务端一路保持到这里,客户端据此隐藏歌词区,不必把它当故障处理。
pub async fn lyric(
    track_id: &str,
) -> Result<LyricDto, ApiError> {
    platform::get_json(lyric_url(track_id)).await
}

/// `POST /register` —— 凭邀请码开户,顺带拿到会话。
///
/// 成功即记住 token,调用方不必再单独调 [`session::set`] —— 那一步漏了的现象是
/// 「注册成功但接着全是 401」,而人会去查服务端。
pub async fn register(
    username: &str,
    password: &str,
    invite: &str,
) -> Result<SessionDto, ApiError> {
    let dto: SessionDto = platform::send_json(
        reqwest::Method::POST,
        format!("{}/register", base_url()),
        Some(RegisterDto {
            username: username.to_owned(),
            password: password.to_owned(),
            invite: invite.to_owned(),
        }),
    )
    .await?;

    session::set(&dto.token);

    Ok(dto)
}

/// `POST /login` —— 用用户名密码换会话,同样自动记住 token。
pub async fn login(
    username: &str,
    password: &str,
) -> Result<SessionDto, ApiError> {
    let dto: SessionDto = platform::send_json(
        reqwest::Method::POST,
        format!("{}/login", base_url()),
        Some(LoginDto {
            username: username.to_owned(),
            password: password.to_owned(),
        }),
    )
    .await?;

    session::set(&dto.token);

    Ok(dto)
}

/// `POST /logout` —— 吊销这一条会话。
///
/// 本地那份**无论服务端怎么答都要清掉**:请求失败时用户的意图仍然是登出,
/// 留着一个可能已失效的 token 只会让下一次操作莫名其妙地 401。
pub async fn logout() -> Result<(), ApiError> {
    let result = platform::send_no_content::<()>(
        reqwest::Method::POST,
        format!("{}/logout", base_url()),
        None,
    )
    .await;

    session::clear();

    result
}

/// `GET /playlists` —— 两个来源合并后的歌单列表,「我喜欢的」在最前。
pub async fn playlists() -> Result<PlaylistsDto, ApiError> {
    platform::get_json(format!("{}/playlists", base_url()))
        .await
}

/// `POST /playlists` —— 建一个本地歌单。
pub async fn create_playlist(
    name: &str,
) -> Result<PlaylistDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        format!("{}/playlists", base_url()),
        Some(Named {
            name: name.to_owned(),
        }),
    )
    .await
}

/// `PATCH /playlists/{id}` —— 给本地歌单改名。
pub async fn rename_playlist(
    id: &str,
    name: &str,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::PATCH,
        playlist_url(id),
        Some(Named {
            name: name.to_owned(),
        }),
    )
    .await
}

/// `DELETE /playlists/{id}` —— 删掉本地歌单。
pub async fn delete_playlist(
    id: &str,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        reqwest::Method::DELETE,
        playlist_url(id),
        None,
    )
    .await
}

/// `GET /playlists/local/{id}/tracks` —— 本地歌单的曲目。
pub async fn playlist_tracks(
    id: &str,
) -> Result<TracksDto, ApiError> {
    platform::get_json(playlist_tracks_url(id)).await
}

/// `GET /playlists/platform/{id}/tracks` —— 平台歌单的曲目。
///
/// 与本地那条是两个函数而不是一个带来源参数的:调用方在点开一个歌单时
/// 就已经知道它是哪一种(列表里的 `source` 就是),合成一个只会让每个
/// 调用点先去问一遍。
pub async fn platform_playlist_tracks(
    id: &str,
) -> Result<TracksDto, ApiError> {
    platform::get_json(platform_playlist_tracks_url(id))
        .await
}

/// `POST /playlists/{id}/tracks` —— 往本地歌单加曲目。
pub async fn add_playlist_tracks(
    id: &str,
    tracks: &[(String, String)],
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::POST,
        playlist_tracks_url(id),
        Some(TrackRefs::from(tracks)),
    )
    .await
}

/// `DELETE /playlists/{id}/tracks` —— 从本地歌单移掉曲目。
pub async fn remove_playlist_tracks(
    id: &str,
    tracks: &[(String, String)],
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::DELETE,
        playlist_tracks_url(id),
        Some(TrackRefs::from(tracks)),
    )
    .await
}

/// `PUT|DELETE /liked/{track_id}` —— 点红心或取消。
pub async fn set_liked(
    track_id: &str,
    liked: bool,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        toggle_method(liked),
        liked_url(track_id),
        None,
    )
    .await
}

/// `PUT|DELETE /subscriptions/playlists/{id}` —— 收藏平台歌单或取消。
pub async fn set_subscribed(
    playlist_id: &str,
    subscribed: bool,
) -> Result<(), ApiError> {
    platform::send_no_content::<()>(
        toggle_method(subscribed),
        subscription_url(playlist_id),
        None,
    )
    .await
}

/// `POST /played` —— 报告一次起播。
///
/// 在声音真的出来之后才调,不是按下播放键就调:取直链可能失败,
/// 那时并没有发生一次播放。
pub async fn record_play(
    platform_name: &str,
    track_id: &str,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::POST,
        format!("{}/played", base_url()),
        Some(PlayedDto {
            platform: platform_name.to_owned(),
            track_id: track_id.to_owned(),
        }),
    )
    .await
}

/// `GET /recent` —— 最近播放。
pub async fn recent() -> Result<TracksDto, ApiError> {
    platform::get_json(format!("{}/recent", base_url()))
        .await
}

/// 只有一个 `name` 字段的请求体,建歌单与改名共用。
#[derive(Serialize)]
struct Named {
    name: String,
}

/// 增删曲目的请求体。
#[derive(Serialize)]
struct TrackRefs {
    tracks: Vec<TrackRefDto>,
}

#[derive(Serialize)]
struct TrackRefDto {
    platform: String,
    id: String,
}

impl TrackRefs {
    fn from(tracks: &[(String, String)]) -> Self {
        Self {
            tracks: tracks
                .iter()
                .map(|(platform, id)| TrackRefDto {
                    platform: platform.clone(),
                    id: id.clone(),
                })
                .collect(),
        }
    }
}

/// 开与关只差一个方法名。写成一处,免得两个端点各写一遍再有一处写反。
fn toggle_method(on: bool) -> reqwest::Method {
    if on {
        reqwest::Method::PUT
    } else {
        reqwest::Method::DELETE
    }
}

/// 本地歌单的地址。路径里带上 `local`,因为两种歌单的 id **不在同一个空间**:
/// 本地是整数主键,平台是平台自己的字符串 id。混了的现象是「查无此歌单」,
/// 看起来像数据没了。
fn playlist_url(id: &str) -> String {
    format!(
        "{}/playlists/local/{}",
        base_url(),
        encode_component(id)
    )
}

fn playlist_tracks_url(id: &str) -> String {
    format!("{}/tracks", playlist_url(id))
}

/// 平台歌单曲目的地址。
fn platform_playlist_tracks_url(id: &str) -> String {
    format!(
        "{}/playlists/platform/{}/tracks",
        base_url(),
        encode_component(id)
    )
}

fn liked_url(track_id: &str) -> String {
    format!(
        "{}/liked/{}",
        base_url(),
        encode_component(track_id)
    )
}

fn subscription_url(playlist_id: &str) -> String {
    format!(
        "{}/subscriptions/playlists/{}",
        base_url(),
        encode_component(playlist_id)
    )
}

/// 拉取任意 URL 的原始字节(封面图这类二进制资源)。
///
/// 与 `play_source` 的直链同一注意事项:封面 URL 指向平台 CDN,可能过期或
/// 返回 HTML 错误页 —— 调用方必须把「字节不是图」当常态处理,不能 panic。
pub async fn fetch_bytes(
    url: &str,
) -> Result<Vec<u8>, ApiError> {
    platform::get_bytes(url.to_owned()).await
}

/// 把服务端的错误响应体翻成一个带 code 的错误。
///
/// 解不出 [`ErrorDto`] 就退回 [`ApiError::Transport`] —— 502 网关回的是 HTML,
/// 反向代理回的可能是别的东西。编一个 code 出来会让上层按错误的分支走,
/// 而那种错比"不知道为什么失败"更难查。
fn server_error(status: u16, body: &str) -> ApiError {
    match serde_json::from_str::<ErrorDto>(body) {
        Ok(dto) => ApiError::Server {
            code: dto.code,
            message: dto.message,
        },
        Err(_) => ApiError::Transport(format!(
            "HTTP {status}: {}",
            body.chars().take(200).collect::<String>()
        )),
    }
}

/// 会话:登录之后拿到的 token,以及它的落盘。
///
/// token 归本 crate 而不是 `app-core`:它是「怎么发请求」的一部分,
/// 而客户端领域按 `CONTEXT.md` 不认识网络。
pub mod session {
    use std::sync::RwLock;

    /// 当前会话的 token。没登录时是 `None`。
    ///
    /// 全局可变状态在这里是恰当的:一个进程只有一个登录态,
    /// 而每一次请求都要用到它 —— 层层传递只会让每个函数都多一个参数。
    static TOKEN: RwLock<Option<String>> =
        RwLock::new(None);

    /// 当前 token 的副本。
    pub fn token() -> Option<String> {
        TOKEN.read().ok().and_then(|slot| slot.clone())
    }

    /// 记住一个 token(登录成功后),并落盘。
    pub fn set(token: &str) {
        if let Ok(mut slot) = TOKEN.write() {
            *slot = Some(token.to_owned());
        }
        super::platform::save_session(Some(token));
    }

    /// 忘掉 token(登出),并清掉落盘的那份。
    pub fn clear() {
        if let Ok(mut slot) = TOKEN.write() {
            *slot = None;
        }
        super::platform::save_session(None);
    }

    /// 从落盘处恢复上次的登录态。各端入口在启动时调一次。
    ///
    /// 恢复出来的 token 可能已经被服务端吊销 —— 那不是这里能知道的事,
    /// 第一次带着它请求时会得到 401,界面据此回到登录页。
    pub fn restore() {
        if let Some(saved) = super::platform::load_session()
            && let Ok(mut slot) = TOKEN.write()
        {
            *slot = Some(saved);
        }
    }
}

/// 唯一按 target 分叉的地方。两个实现的**签名相同**,差异不外泄。
#[cfg(not(target_arch = "wasm32"))]
mod platform {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use serde::de::DeserializeOwned;
    use tokio::runtime::Runtime;

    use super::ApiError;

    /// 后台多线程 tokio runtime,专门用来跑 IO。
    ///
    /// 它是 `Send` 约束的**唯一**来源:`Runtime::spawn` 要求 future 是 `Send`。
    /// reqwest 的 future 满足这一点,而 `app-core` 的 future 不必满足 —— 因为
    /// 它们从不经过这里。见 `docs/adr/0002`。
    fn runtime() -> &'static Runtime {
        static RUNTIME: OnceLock<Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| {
            Runtime::new()
                .expect("failed to start tokio runtime")
        })
    }

    /// 一次请求最多等多久。与 [`get_bytes`] 取同一个值。
    ///
    /// **没有超时等于没有失败**:`reqwest::get` 默认不设超时,断网时那个 future
    /// 会永远悬着 —— 点一首歌永远停在「加载中」,断流后的探测永远问不出结果,
    /// 而横幅在等那个结果(见 `docs/adr/0013`)。宁可在十秒处认输。
    const REQUEST_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(10);

    pub(super) async fn get_json<
        T: DeserializeOwned + Send + 'static,
    >(
        url: String,
    ) -> Result<T, ApiError> {
        send_json::<(), T>(reqwest::Method::GET, url, None)
            .await
    }

    /// 一次带请求体、带登录态的往返,并解码响应。
    ///
    /// 登录态在这里统一附上,而不是每个端点各自记得加 ——
    /// 漏一处的现象是那条路由 401,而那时人会去查服务端。
    pub(super) async fn send_json<
        B: serde::Serialize + Send + 'static,
        T: DeserializeOwned + Send + 'static,
    >(
        method: reqwest::Method,
        url: String,
        body: Option<B>,
    ) -> Result<T, ApiError> {
        let response = send(method, url, body).await?;

        response
            .json::<T>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// 同上,但不看响应体 —— 写操作服务端回 204,那里没有内容可解。
    pub(super) async fn send_no_content<
        B: serde::Serialize + Send + 'static,
    >(
        method: reqwest::Method,
        url: String,
        body: Option<B>,
    ) -> Result<(), ApiError> {
        send(method, url, body).await.map(|_| ())
    }

    /// 发出去、检查状态码,响应原样交给调用方。
    async fn send<B: serde::Serialize + Send + 'static>(
        method: reqwest::Method,
        url: String,
        body: Option<B>,
    ) -> Result<reqwest::Response, ApiError> {
        let token = super::session::token();

        // spawn 把请求丢到后台线程池;await 的是 JoinHandle,它可以在任意
        // 线程上被 poll —— 包括 slint 的 UI 线程。
        runtime()
            .spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(REQUEST_TIMEOUT)
                    .build()
                    .map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })?;

                let mut request =
                    client.request(method, url);
                if let Some(token) = token {
                    request = request.bearer_auth(token);
                }
                if let Some(body) = body {
                    request = request.json(&body);
                }

                let response =
                    request.send().await.map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })?;

                check(response).await
            })
            .await
            .map_err(|join_error| {
                ApiError::Transport(join_error.to_string())
            })?
    }

    /// 非 2xx 时把响应体读出来,好让服务端给的 code 活到调用方手里。
    ///
    /// `error_for_status` 做不到这件事:它只看状态码,响应体连同里面的 code
    /// 一起被丢掉,上层就只能拿到一句"HTTP 401"。
    async fn check(
        response: reqwest::Response,
    ) -> Result<reqwest::Response, ApiError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let body =
            response.text().await.unwrap_or_default();

        Err(super::server_error(status.as_u16(), &body))
    }

    /// 会话文件的位置。可用 `SLINT_STUDY_SESSION_FILE` 直接指定。
    ///
    /// 走 `XDG_STATE_HOME` 而不是配置目录:登录态是**状态**不是配置,
    /// 它不该被同步、也不该被人手写。
    fn session_file() -> Option<PathBuf> {
        if let Ok(explicit) =
            std::env::var("SLINT_STUDY_SESSION_FILE")
        {
            return Some(PathBuf::from(explicit));
        }

        session_path_from(
            std::env::var("XDG_STATE_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )
    }

    /// 由环境算出会话文件的路径。抽成纯函数才测得到 ——
    /// 直接读环境变量的话,测试之间会互相干扰。
    ///
    /// 两个变量都没有时返回 `None` 而不是猜一个路径:安卓上就是这种情况,
    /// 那里的私有目录要走 JNI 才拿得到。猜错了写进去,失败还是静默的。
    pub(super) fn session_path_from(
        state_home: Option<&str>,
        home: Option<&str>,
    ) -> Option<PathBuf> {
        let base = match (state_home, home) {
            (Some(state), _) if !state.is_empty() => {
                PathBuf::from(state)
            }
            (_, Some(home)) if !home.is_empty() => {
                PathBuf::from(home).join(".local/state")
            }
            _ => return None,
        };

        Some(base.join("slint-study/session"))
    }

    /// 落盘的 token,没有就是没登录过。
    pub(super) fn load_session() -> Option<String> {
        let path = session_file()?;
        let saved = std::fs::read_to_string(path).ok()?;
        let saved = saved.trim();

        (!saved.is_empty()).then(|| saved.to_owned())
    }

    /// 存一个 token,`None` 表示登出 —— 那要把文件删掉,
    /// 而不是写一个空文件:留着一个空文件等于留着一份"曾经登录过"的痕迹。
    pub(super) fn save_session(token: Option<&str>) {
        let Some(path) = session_file() else {
            return;
        };

        let Some(token) = token else {
            let _ = std::fs::remove_file(&path);
            return;
        };

        write_session(&path, token);
    }

    /// 写会话文件。权限 0600 —— token 等同于密码。
    ///
    /// 失败只记一笔:登录本身已经成功了,存不下来的后果是下次要重登,
    /// 不该让它把这次登录也判为失败。
    pub(super) fn write_session(path: &Path, token: &str) {
        if let Some(parent) = path.parent()
            && let Err(err) =
                std::fs::create_dir_all(parent)
        {
            log::warn!("建会话目录失败: {err}");
            return;
        }

        if let Err(err) = std::fs::write(path, token) {
            log::warn!("写会话失败: {err}");
            return;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if let Err(err) = std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(0o600),
            ) {
                log::warn!("设置会话文件权限失败: {err}");
            }
        }
    }

    /// 同 [`get_json`],但不解码,原样给字节。
    ///
    /// 带显式超时:这类 URL 指向外部 CDN(封面图),可能整段不可达 ——
    /// 实测网易 CDN 从部分网络直连会**无响应挂死**而不是拒绝。`reqwest::get`
    /// 默认没有超时,不设的话这个 future 永远悬着。
    pub(super) async fn get_bytes(
        url: String,
    ) -> Result<Vec<u8>, ApiError> {
        runtime()
            .spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(
                        std::time::Duration::from_secs(10),
                    )
                    .build()
                    .map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })?;
                let response = client
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })?
                    .error_for_status()
                    .map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })?;
                response
                    .bytes()
                    .await
                    .map(|b| b.to_vec())
                    .map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })
            })
            .await
            .map_err(|join_error| {
                ApiError::Transport(join_error.to_string())
            })?
    }
}

/// wasm 上没有线程,请求由浏览器的 fetch 驱动;future 不是 `Send`,无所谓。
///
/// 这一侧不设超时,与原生那边不对称是有意的:reqwest 的 wasm 客户端不支持
/// `timeout`(fetch 的时限归浏览器管),而 wasm 上根本没有音频栈,
/// 断流那条路走不到这里。
#[cfg(target_arch = "wasm32")]
mod platform {
    use serde::de::DeserializeOwned;

    use super::ApiError;

    /// localStorage 里存会话用的键。
    const SESSION_KEY: &str = "slint-study.session";

    pub(super) async fn get_json<T: DeserializeOwned>(
        url: String,
    ) -> Result<T, ApiError> {
        send_json::<(), T>(reqwest::Method::GET, url, None)
            .await
    }

    /// 一次带请求体、带登录态的往返,并解码响应。
    pub(super) async fn send_json<
        B: serde::Serialize,
        T: DeserializeOwned,
    >(
        method: reqwest::Method,
        url: String,
        body: Option<B>,
    ) -> Result<T, ApiError> {
        let response = send(method, url, body).await?;

        response
            .json::<T>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// 同上,但不看响应体 —— 写操作服务端回 204。
    pub(super) async fn send_no_content<
        B: serde::Serialize,
    >(
        method: reqwest::Method,
        url: String,
        body: Option<B>,
    ) -> Result<(), ApiError> {
        send(method, url, body).await.map(|_| ())
    }

    async fn send<B: serde::Serialize>(
        method: reqwest::Method,
        url: String,
        body: Option<B>,
    ) -> Result<reqwest::Response, ApiError> {
        let mut request =
            reqwest::Client::new().request(method, url);

        if let Some(token) = super::session::token() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response =
            request.send().await.map_err(|e| {
                ApiError::Transport(e.to_string())
            })?;

        check(response).await
    }

    /// 非 2xx 时把响应体读出来,好让服务端给的 code 活到调用方手里。
    ///
    /// `error_for_status` 做不到这件事:它只看状态码,响应体连同里面的 code
    /// 一起被丢掉,上层就只能拿到一句"HTTP 401"。
    async fn check(
        response: reqwest::Response,
    ) -> Result<reqwest::Response, ApiError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let body =
            response.text().await.unwrap_or_default();

        Err(super::server_error(status.as_u16(), &body))
    }

    /// 浏览器的 localStorage。取不到(隐私模式、没有 window)就当没有会话 ——
    /// 那只意味着刷新后要重登,不是故障。
    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub(super) fn load_session() -> Option<String> {
        let saved =
            storage()?.get_item(SESSION_KEY).ok()??;

        (!saved.is_empty()).then_some(saved)
    }

    pub(super) fn save_session(token: Option<&str>) {
        let Some(storage) = storage() else {
            return;
        };

        let _ = match token {
            Some(token) => {
                storage.set_item(SESSION_KEY, token)
            }
            // 登出要删掉,不是写空串:空串等于留着一份"曾经登录过"的痕迹
            None => storage.remove_item(SESSION_KEY),
        };
    }

    /// 同 [`get_json`],但不解码,原样给字节。
    pub(super) async fn get_bytes(
        url: String,
    ) -> Result<Vec<u8>, ApiError> {
        let response = reqwest::get(url)
            .await
            .map_err(|e| {
                ApiError::Transport(e.to_string())
            })?
            .error_for_status()
            .map_err(|e| {
                ApiError::Transport(e.to_string())
            })?;
        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| ApiError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dto(protocol_version: u32) -> HealthDto {
        HealthDto {
            status: "ok".to_owned(),
            protocol_version,
        }
    }

    /// 版本一致时原样放行,且不吞掉 dto 的其他字段。
    #[test]
    fn matching_version_returns_dto() {
        let ok = check_version(dto(PROTOCOL_VERSION))
            .expect("版本一致时不应报错");
        assert_eq!(ok.protocol_version, PROTOCOL_VERSION);
        assert_eq!(ok.status, "ok");
    }

    /// 客户端旧、服务端新:必须报错而不是接受。
    #[test]
    fn newer_server_version_is_mismatch() {
        let err = check_version(dto(PROTOCOL_VERSION + 1))
            .expect_err("版本更高时应报错");
        assert!(matches!(
            err,
            ApiError::VersionMismatch { expected, actual }
                if expected == PROTOCOL_VERSION
                    && actual == PROTOCOL_VERSION + 1
        ));
    }

    /// 客户端新、服务端旧:不匹配是对称的,不存在"向后兼容就放行"。
    #[test]
    fn older_server_version_is_mismatch() {
        let older = PROTOCOL_VERSION - 1;
        let err = check_version(dto(older))
            .expect_err("版本更低时应报错");
        assert!(matches!(
            err,
            ApiError::VersionMismatch { expected, actual }
                if expected == PROTOCOL_VERSION && actual == older
        ));
    }

    /// 边界:服务端给了 0 —— 正是 `health` 注释里说的"字段静默变成默认值"的场景,
    /// 必须被拦下而不是当成合法版本。
    ///
    /// 注意:当前实现下它与上一个用例走同一条 `!=` 分支,并非靠红转绿证明。
    /// 留着它是为了钉住意图:将来若有人把校验改成"仅当 actual > expected 才报错",
    /// 这里会红。
    #[test]
    fn zero_server_version_is_mismatch() {
        let err = check_version(dto(0))
            .expect_err("版本为 0 时应报错");
        assert!(matches!(
            err,
            ApiError::VersionMismatch { actual: 0, .. }
        ));
    }

    /// 关键词里的 `&`、空格、中文都必须转义。
    ///
    /// 不转义的话 `q=a&b` 会被服务端解析成两个参数,关键词静默变成半截 ——
    /// 不报错、不失败,只是搜出来的东西不对。
    #[test]
    fn search_url_percent_encodes_keyword() {
        let url = search_url("tracks", "紅蓮華 & LiSA");

        assert!(
            url.ends_with(
                "/search/tracks?q=%E7%B4%85%E8%93%AE%E8%8F%AF%20%26%20LiSA"
            ),
            "关键词没被完整转义: {url}"
        );
    }

    /// 路径拼接:id 原样落在 `/play/` 之后。
    #[test]
    fn play_url_contains_track_id() {
        assert!(
            play_url("1375305989")
                .ends_with("/play/1375305989"),
            "id 没落在路径末尾"
        );
    }

    /// 歌词地址与播放地址同构,id 一样要落在路径末尾。
    #[test]
    fn lyric_url_contains_track_id() {
        assert!(
            lyric_url("1375305989")
                .ends_with("/lyric/1375305989"),
            "id 没落在路径末尾"
        );
    }

    /// 钉住会显示给用户的那句文案。措辞一改这里就红 —— 这是特性:
    /// 文案里的每个汉字都必须在 `crates/ui/fonts/cjk-subset.ttf` 里,
    /// 改了措辞就得重跑 `just font-subset`,否则 web 端显示成豆腐块。
    #[test]
    fn mismatch_message_contains_both_versions() {
        let err = ApiError::VersionMismatch {
            expected: 1,
            actual: 2,
        };
        assert_eq!(
            err.to_string(),
            "协议版本不匹配: 本机 v1,服务端 v2"
        );
    }

    /// 会话 token 的一生:一开始没有,登录后有,换账号后是新的,登出后又没有。
    ///
    /// 四件事写在**一个**测试里而不是四个:token 是进程级的全局状态,
    /// 拆成四个测试会并行地互相踩,而「一开始没有」那条还依赖执行顺序。
    #[test]
    fn the_session_token_has_a_lifecycle() {
        // 用一个临时文件当会话落盘处,免得动到真实的那一份
        let dir = std::env::temp_dir()
            .join("slint-study-session-lifecycle");
        let _ = std::fs::create_dir_all(&dir);
        // SAFETY: 单线程测试起点,此时还没有别的线程在读环境
        unsafe {
            std::env::set_var(
                "SLINT_STUDY_SESSION_FILE",
                dir.join("session"),
            );
        }

        session::clear();
        assert_eq!(
            session::token(),
            None,
            "一开始不该有 token"
        );

        session::set("first");
        assert_eq!(
            session::token().as_deref(),
            Some("first")
        );

        session::set("second");
        assert_eq!(
            session::token().as_deref(),
            Some("second"),
            "换账号登录后带的该是新 token"
        );

        session::clear();
        assert_eq!(
            session::token(),
            None,
            "登出后不该还留着"
        );
    }

    /// 有 XDG_STATE_HOME 就用它 —— 登录态是状态不是配置。
    #[test]
    fn session_path_prefers_state_home() {
        let path = platform::session_path_from(
            Some("/tmp/state"),
            Some("/home/someone"),
        )
        .expect("给了 state home 就该有路径");

        assert!(path.starts_with("/tmp/state"));
        assert!(path.ends_with("slint-study/session"));
    }

    /// 没有 XDG_STATE_HOME 就退到 HOME/.local/state。
    #[test]
    fn session_path_falls_back_to_home() {
        let path = platform::session_path_from(
            None,
            Some("/home/someone"),
        )
        .expect("有 HOME 就该有路径");

        assert!(
            path.starts_with("/home/someone/.local/state")
        );
    }

    /// 两个都没有时不猜一个路径出来 —— 安卓上就是这种情况,
    /// 猜错了写进去,失败还是静默的。空串等同于没有。
    #[test]
    fn session_path_is_none_without_either() {
        assert_eq!(
            platform::session_path_from(None, None),
            None
        );
        assert_eq!(
            platform::session_path_from(Some(""), Some("")),
            None
        );
    }

    /// 存了再读,拿回同一个 token —— 这是"下次启动还登着"的全部含义。
    #[test]
    fn session_survives_a_restart() {
        let path = std::env::temp_dir()
            .join("slint-study-session-restart/session");
        platform::write_session(&path, "kept");

        let read = std::fs::read_to_string(&path)
            .expect("刚写的文件该读得到");

        assert_eq!(read.trim(), "kept");
        let _ = std::fs::remove_file(&path);
    }

    /// 会话文件权限是 0600 —— token 等同于密码。
    #[cfg(unix)]
    #[test]
    fn session_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::env::temp_dir()
            .join("slint-study-session-perm/session");
        platform::write_session(&path, "secret");

        let mode = std::fs::metadata(&path)
            .expect("刚写的文件该在")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600, "会话文件权限应为 0600");
        let _ = std::fs::remove_file(&path);
    }

    /// 歌单相关的三种地址各自成形,且 id 进路径要转义 ——
    /// 不转义的话,一个带斜杠的 id 会把路径截成另一条路由。
    #[test]
    fn playlist_urls_are_built_per_kind() {
        assert!(
            playlist_url("3")
                .ends_with("/playlists/local/3"),
            "实际 {}",
            playlist_url("3")
        );
        assert!(
            playlist_tracks_url("3")
                .ends_with("/playlists/local/3/tracks")
        );
        // 两种来源走两条路径 —— 混了的现象是「查无此歌单」,看着像数据没了
        assert!(
            platform_playlist_tracks_url("24381616")
                .ends_with(
                    "/playlists/platform/24381616/tracks"
                )
        );
        assert!(subscription_url("24381616").ends_with(
            "/subscriptions/playlists/24381616"
        ));
        assert!(
            liked_url("347230").ends_with("/liked/347230")
        );
    }

    /// id 里的斜杠与空格都要转义。
    #[test]
    fn track_ids_are_escaped_in_paths() {
        assert!(
            liked_url("a/b c")
                .ends_with("/liked/a%2Fb%20c")
        );
        assert!(
            playlist_url("a/b")
                .ends_with("/playlists/local/a%2Fb")
        );
        // 平台 id 来自平台,更该转义:它可能带任何字符
        assert!(
            platform_playlist_tracks_url("a/b").ends_with(
                "/playlists/platform/a%2Fb/tracks"
            )
        );
    }

    /// 服务端回的 code 保留进错误里,不被压成一句文本 ——
    /// 契约里那些 code 存在的全部意义就是给客户端分支用的。
    #[test]
    fn server_error_body_keeps_its_code() {
        let err = server_error(
            401,
            r#"{"code":"bad_credentials","message":"用户名或密码不对"}"#,
        );

        assert!(matches!(
            &err,
            ApiError::Server { code, message }
                if code == "bad_credentials"
                    && message == "用户名或密码不对"
        ));
    }

    /// 解不出 ErrorDto 时退回 Transport,不编一个 code 出来。
    /// 编了会让上层按错误的分支走,而那种错比"不知道为什么失败"更难查。
    #[test]
    fn unparseable_error_body_falls_back_to_transport() {
        let err = server_error(
            502,
            "<html><body>Bad Gateway</body></html>",
        );

        assert!(
            matches!(err, ApiError::Transport(_)),
            "实际 {err:?}"
        );
    }
}
