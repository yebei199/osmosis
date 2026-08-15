use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::de::DeserializeOwned;
use tokio::runtime::Runtime;

use crate::ApiError;

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

pub(crate) async fn get_json<
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
pub(crate) async fn send_json<
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
pub(crate) async fn send_no_content<
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
    let token = crate::session::token();

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

            let mut request = client.request(method, url);
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

    let body = response.text().await.unwrap_or_default();

    Err(crate::server_error(status.as_u16(), &body))
}

/// 会话文件的位置。可用 `OSMOSIS_SESSION_FILE` 直接指定。
///
/// 走 `XDG_STATE_HOME` 而不是配置目录:登录态是**状态**不是配置,
/// 它不该被同步、也不该被人手写。
fn session_file() -> Option<PathBuf> {
    if let Ok(explicit) =
        std::env::var("OSMOSIS_SESSION_FILE")
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
pub(crate) fn session_path_from(
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

    Some(base.join("osmosis/session"))
}

/// 本地设置文件,与会话文件同一个目录。
///
/// 两者分开放而不是塞进一份:token 是凭据,权限 0600、登出即删;设置是偏好,
/// 登出之后照样该留着。合成一个文件的话,登出会顺手把音量也忘掉。
fn settings_file() -> Option<PathBuf> {
    if let Ok(explicit) =
        std::env::var("OSMOSIS_SETTINGS_FILE")
    {
        return Some(PathBuf::from(explicit));
    }

    session_path_from(
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .map(|path| path.with_file_name("settings.json"))
}

/// 封面缓存目录,与会话、设置同一个基座。
fn artwork_dir() -> Option<PathBuf> {
    session_path_from(
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .map(|path| path.with_file_name("covers"))
}

pub(crate) fn load_artwork(name: &str) -> Option<Vec<u8>> {
    std::fs::read(artwork_dir()?.join(name)).ok()
}

pub(crate) fn save_artwork(name: &str, bytes: &[u8]) {
    let Some(dir) = artwork_dir() else {
        return;
    };
    write_artwork(&dir, name, bytes);
}

/// 曲目缩略图目录。挂在封面目录下面而不是并列一个新目录:它们是同一类
/// 东西,只是键与淘汰规则不同(见 `crate::TRACK_ARTWORK_BUDGET`)。
fn track_artwork_dir() -> Option<PathBuf> {
    artwork_dir().map(|dir| dir.join("tracks"))
}

pub(crate) fn load_track_artwork(
    name: &str,
) -> Option<Vec<u8>> {
    std::fs::read(track_artwork_dir()?.join(name)).ok()
}

pub(crate) fn save_track_artwork(name: &str, bytes: &[u8]) {
    let Some(dir) = track_artwork_dir() else {
        return;
    };
    write_artwork(&dir, name, bytes);
}

pub(crate) fn sweep_track_artwork(budget: u64) {
    let Some(dir) = track_artwork_dir() else {
        return;
    };
    sweep_dir(&dir, budget);
}

/// 往某个封面目录里写一份。目录不存在就建。
fn write_artwork(dir: &Path, name: &str, bytes: &[u8]) {
    if let Err(err) = std::fs::create_dir_all(dir) {
        log::warn!("建封面目录失败: {err}");
        return;
    }

    if let Err(err) = std::fs::write(dir.join(name), bytes)
    {
        log::warn!("写封面失败: {err}");
    }
}

/// 目录超出预算时按 mtime 从最旧的删起,删到线下为止。
///
/// 单独一个函数是为了能对着临时目录测 —— 上面那几个都要先解出
/// `XDG_STATE_HOME`,测起来就成了改进程环境变量。
pub(crate) fn sweep_dir(dir: &Path, budget: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // 目录还不存在:第一次运行,没什么可删的
        return;
    };

    let mut files: Vec<(
        std::time::SystemTime,
        u64,
        PathBuf,
    )> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((
                meta.modified().ok()?,
                meta.len(),
                entry.path(),
            ))
        })
        .collect();

    let mut total: u64 =
        files.iter().map(|(_, len, _)| len).sum();
    if total <= budget {
        return;
    }

    // 最旧的排在前面 —— 删的就是这一头
    files.sort_by_key(|(modified, _, _)| *modified);

    for (_, len, path) in files {
        if total <= budget {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// 读设置文件的原文。读不到就是没有 —— 解析那半归 `settings` 模块。
pub(crate) fn load_settings() -> Option<String> {
    std::fs::read_to_string(settings_file()?).ok()
}

/// 写设置文件。失败只记一笔:调音量本身已经生效了,
/// 存不下的后果是下次回到默认值,不该让它把这次也判为失败。
pub(crate) fn save_settings(raw: &str) {
    let Some(path) = settings_file() else {
        return;
    };

    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        log::warn!("建设置目录失败: {err}");
        return;
    }

    if let Err(err) = std::fs::write(&path, raw) {
        log::warn!("写设置失败: {err}");
    }
}

/// 落盘的 token,没有就是没登录过。
pub(crate) fn load_session() -> Option<String> {
    let path = session_file()?;
    let saved = std::fs::read_to_string(path).ok()?;
    let saved = saved.trim();

    (!saved.is_empty()).then(|| saved.to_owned())
}

/// 存一个 token,`None` 表示登出 —— 那要把文件删掉,
/// 而不是写一个空文件:留着一个空文件等于留着一份"曾经登录过"的痕迹。
pub(crate) fn save_session(token: Option<&str>) {
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
pub(crate) fn write_session(path: &Path, token: &str) {
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
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
pub(crate) async fn get_bytes(
    url: String,
) -> Result<Vec<u8>, ApiError> {
    runtime()
        .spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
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

#[cfg(test)]
mod tests;
