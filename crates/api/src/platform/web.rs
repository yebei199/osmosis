use serde::de::DeserializeOwned;

use crate::ApiError;

/// localStorage 里存会话用的键。
const SESSION_KEY: &str = "osmosis.session";

/// localStorage 里存本地设置用的键。
///
/// 与会话分成两个键:登出要删掉会话,而音量该留着。
const SETTINGS_KEY: &str = "osmosis.settings";

pub(crate) fn load_settings() -> Option<String> {
    storage()?.get_item(SETTINGS_KEY).ok()?
}

/// web 上不缓存封面:localStorage 只存文本,而把图片编成 base64 塞进去
/// 会撞上 5MB 的配额 —— 那额度是留给会话与设置的。浏览器自己的 HTTP 缓存
/// 已经在做这件事,再来一层是白费。
pub(crate) fn load_artwork(_name: &str) -> Option<Vec<u8>> {
    None
}

pub(crate) fn save_artwork(_name: &str, _bytes: &[u8]) {}

pub(crate) fn load_track_artwork(
    _name: &str,
) -> Option<Vec<u8>> {
    None
}

pub(crate) fn save_track_artwork(
    _name: &str,
    _bytes: &[u8],
) {
}

/// 没有磁盘缓存也就没什么可清 —— 浏览器自己的 HTTP 缓存在做这件事。
pub(crate) fn sweep_track_artwork(_budget: u64) {}

pub(crate) fn save_settings(raw: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(SETTINGS_KEY, raw);
    }
}

pub(crate) async fn get_json<T: DeserializeOwned>(
    url: String,
) -> Result<T, ApiError> {
    send_json::<(), T>(reqwest::Method::GET, url, None)
        .await
}

/// 一次带请求体、带登录态的往返,并解码响应。
pub(crate) async fn send_json<
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
pub(crate) async fn send_no_content<B: serde::Serialize>(
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

    if let Some(token) = crate::session::token() {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }

    let response = request
        .send()
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;

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

    let body = response.text().await.unwrap_or_default();

    Err(crate::server_error(status.as_u16(), &body))
}

/// 浏览器的 localStorage。取不到(隐私模式、没有 window)就当没有会话 ——
/// 那只意味着刷新后要重登,不是故障。
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

pub(crate) fn load_session() -> Option<String> {
    let saved = storage()?.get_item(SESSION_KEY).ok()??;

    (!saved.is_empty()).then_some(saved)
}

pub(crate) fn save_session(token: Option<&str>) {
    let Some(storage) = storage() else {
        return;
    };

    let _ = match token {
        Some(token) => storage.set_item(SESSION_KEY, token),
        // 登出要删掉,不是写空串:空串等于留着一份"曾经登录过"的痕迹
        None => storage.remove_item(SESSION_KEY),
    };
}

/// 同 [`get_json`],但不解码,原样给字节。
pub(crate) async fn get_bytes(
    url: String,
) -> Result<Vec<u8>, ApiError> {
    let response = reqwest::get(url)
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?
        .error_for_status()
        .map_err(|e| ApiError::Transport(e.to_string()))?;
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| ApiError::Transport(e.to_string()))
}
