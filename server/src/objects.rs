//! 对象存储:听过的歌存在这里,再播时从这里交付(#126)。
//!
//! 只有四个动作 —— 存(流式,见 [`Objects::put`])、问在不在、删、签一条给客户端的只读链接。上面的归档逻辑
//! 只认 [`Objects`] 这个 trait,路由测试拿内存实现替掉 S3,不必起容器。
//!
//! S3 那一侧用 `rusty-s3`:它只**签名**、不带 HTTP,字节照旧走依赖树里已有的那份
//! reqwest。完整的 SDK 为这四个动作要拖进一整棵 aws 依赖树。
//!
//! 两个端点分开配:服务端自己的读写走集群内端点(公网那条经 Cloudflare,空 body 的
//! PUT 必 411),交给客户端的链接签在公网端点上 —— 客户端不在集群里。

use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

/// 失败只需要一句能进日志的话:调用方拿它什么也做不了,只能退回网易云。
pub type ObjectResult<T> = Result<T, String>;

/// 要存的字节:一条流,边到边传,内存里只有正在路上的那几块(#147 的 OOM)。
pub type ObjectBody =
    BoxStream<'static, std::io::Result<Bytes>>;

/// 一段已经在内存里的字节当成 [`ObjectBody`]。只给测试和小对象用。
pub fn whole(bytes: impl Into<Bytes>) -> ObjectBody {
    use futures_util::StreamExt;
    futures_util::stream::iter([Ok(bytes.into())]).boxed()
}

/// 归档用得到的全部动作。
///
/// 返回装箱的 future 而不是 `async fn`:后者做不成 `dyn`,而 `AppState` 要在
/// S3 与测试替身之间换着装。
pub trait Objects: Send + Sync {
    /// 把 `body` 写成一个对象,同名覆盖。`length` 是它的总字节数,事前必须知道:
    /// 单个 PUT 要 Content-Length,流少给或多给都算失败,桶里不会留下半截。
    fn put(
        &self,
        key: &str,
        body: ObjectBody,
        length: u64,
        content_type: &'static str,
    ) -> BoxFuture<'_, ObjectResult<()>>;

    /// 对象在不在。「不在」是正常回答,只有问不到才是 `Err`。
    fn exists(
        &self,
        key: &str,
    ) -> BoxFuture<'_, ObjectResult<bool>>;

    /// 删掉。本来就不在也算成功 —— S3 的 DELETE 本身就是这么定义的。
    fn delete(
        &self,
        key: &str,
    ) -> BoxFuture<'_, ObjectResult<()>>;

    /// 一条客户端拿去直接 GET 的链接,带签名、会过期,支持 Range。
    fn presign_get(&self, key: &str) -> String;
}

/// 连得上 S3 所需的全部配置,由环境变量给(见 [`S3Config::from_env`])。
pub struct S3Config {
    /// 服务端自己读写走的端点,生产上是集群内那个。
    pub endpoint: String,
    /// 签给客户端的链接用的端点。没给就与 `endpoint` 相同(本机开发)。
    pub public_endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl S3Config {
    /// 读环境变量。`S3_ENDPOINT` 没设就是**不启用**归档,返回 `None`;
    /// 设了却缺别的,那是部署写错了,直接 panic —— 与 `INVITE_CODE` 同一个态度,
    /// 带着半套配置起来只会在第一次存歌时才报错。
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("S3_ENDPOINT").ok()?;
        let required = |name: &str| {
            std::env::var(name).unwrap_or_else(|_| {
                panic!("设了 S3_ENDPOINT 就必须也设 {name}")
            })
        };

        Some(Self {
            public_endpoint: std::env::var(
                "S3_PUBLIC_ENDPOINT",
            )
            .unwrap_or_else(|_| endpoint.clone()),
            endpoint,
            bucket: required("S3_BUCKET"),
            // RustFS 不在乎区域,但 SigV4 要一个;它自己的默认值就是这个
            region: std::env::var("S3_REGION")
                .unwrap_or_else(|_| "us-east-1".to_owned()),
            access_key_id: required("S3_ACCESS_KEY_ID"),
            secret_access_key: required(
                "S3_SECRET_ACCESS_KEY",
            ),
        })
    }
}

/// 服务端自己用的那几条签名的有效期。签完立刻就发,一分钟只是给时钟偏差留余量。
const INTERNAL_SIGNATURE: Duration =
    Duration::from_secs(60);

/// 给客户端那条链接的有效期。
///
/// 客户端边放边按 Range 取,暂停后继续也还是这一条链接。六小时盖得住任何一首歌
/// 加上一顿饭的暂停;更久的会像网易云直链过期一样,重新点一次就好。
const CLIENT_SIGNATURE: Duration =
    Duration::from_secs(6 * 3600);

/// 问在不在、删这两件小事最多等多久。
///
/// `/play` 每次都要先问一句在不在:RustFS 卡住时,宁可两秒后退回网易云,
/// 也不能让点歌跟着一起卡住。上传不设总时限,理由同 `/download` 的 `CONNECT_TIMEOUT`。
const SMALL_REQUEST: Duration = Duration::from_secs(2);

/// 真的 S3(生产上是 RustFS)。
pub struct S3 {
    internal: Bucket,
    public: Bucket,
    credentials: Credentials,
    http: reqwest::Client,
}

impl S3 {
    pub fn new(config: S3Config) -> Result<Self, String> {
        let bucket = |endpoint: &str| {
            let url = endpoint.parse().map_err(|err| {
                format!(
                    "S3 端点不是合法 URL({endpoint}): {err}"
                )
            })?;
            // 路径风格:RustFS 与本机容器都不给每个桶配子域名
            Bucket::new(
                url,
                UrlStyle::Path,
                config.bucket.clone(),
                config.region.clone(),
            )
            .map_err(|err| format!("S3 桶配置不对: {err}"))
        };

        Ok(Self {
            internal: bucket(&config.endpoint)?,
            public: bucket(&config.public_endpoint)?,
            credentials: Credentials::new(
                config.access_key_id,
                config.secret_access_key,
            ),
            http: reqwest::Client::builder()
                .connect_timeout(SMALL_REQUEST)
                .build()
                .map_err(|err| err.to_string())?,
        })
    }

    /// 桶不在就建。只给测试与本机开发用:生产上的桶由 infra 建好,
    /// 服务端的凭据也只限这一个桶,建不了桶。
    pub async fn ensure_bucket(&self) -> ObjectResult<()> {
        let url = self
            .internal
            .create_bucket(&self.credentials)
            .sign(INTERNAL_SIGNATURE);
        let response = self
            .http
            .put(url)
            .send()
            .await
            .map_err(|err| err.to_string())?;
        // 409 是「已经有了」,正是想要的结果
        match response.status().as_u16() {
            200..=299 | 409 => Ok(()),
            status => Err(format!("建桶返回 {status}")),
        }
    }
}

/// 状态码不是 2xx 就是失败,状态码进错误信息。
fn check(
    response: reqwest::Response,
    what: &str,
) -> ObjectResult<reqwest::Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(format!("{what}返回 {}", response.status()))
    }
}

/// 删对象的结果。
///
/// 404(NoSuchKey)算删掉了:S3 对不存在的键回 204,RustFS 却回 404,而契约是
/// 「本来就不在也算成功」(见 [`Objects::delete`])。不这样判的话,账上有、桶里
/// 没有的那一行每轮清扫都删不掉,永远留着(#147 R-1)。其余非 2xx 才是失败。
fn deleted(
    status: reqwest::StatusCode,
) -> ObjectResult<()> {
    if status.is_success()
        || status == reqwest::StatusCode::NOT_FOUND
    {
        Ok(())
    } else {
        Err(format!("删对象返回 {status}"))
    }
}

impl Objects for S3 {
    fn put(
        &self,
        key: &str,
        body: ObjectBody,
        length: u64,
        content_type: &'static str,
    ) -> BoxFuture<'_, ObjectResult<()>> {
        let url = self
            .internal
            .put_object(Some(&self.credentials), key)
            .sign(INTERNAL_SIGNATURE);
        Box::pin(async move {
            let response = self
                .http
                .put(url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    content_type,
                )
                .header(
                    reqwest::header::CONTENT_LENGTH,
                    length,
                )
                .body(reqwest::Body::wrap_stream(body))
                .send()
                .await
                .map_err(|err| err.to_string())?;
            check(response, "存对象").map(drop)
        })
    }

    fn exists(
        &self,
        key: &str,
    ) -> BoxFuture<'_, ObjectResult<bool>> {
        let url = self
            .internal
            .head_object(Some(&self.credentials), key)
            .sign(INTERNAL_SIGNATURE);
        Box::pin(async move {
            let response = self
                .http
                .head(url)
                .timeout(SMALL_REQUEST)
                .send()
                .await
                .map_err(|err| err.to_string())?;
            if response.status()
                == reqwest::StatusCode::NOT_FOUND
            {
                return Ok(false);
            }
            check(response, "查对象").map(|_| true)
        })
    }

    fn delete(
        &self,
        key: &str,
    ) -> BoxFuture<'_, ObjectResult<()>> {
        let url = self
            .internal
            .delete_object(Some(&self.credentials), key)
            .sign(INTERNAL_SIGNATURE);
        Box::pin(async move {
            let response = self
                .http
                .delete(url)
                .timeout(SMALL_REQUEST)
                .send()
                .await
                .map_err(|err| err.to_string())?;
            deleted(response.status())
        })
    }

    fn presign_get(&self, key: &str) -> String {
        self.public
            .get_object(Some(&self.credentials), key)
            .sign(CLIENT_SIGNATURE)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::deleted;

    /// 删掉了(2xx)与本来就不在(404)都算成功;5xx 与拒绝访问才是失败,留给下一轮。
    #[test]
    fn a_missing_object_counts_as_deleted() {
        assert_eq!(deleted(StatusCode::NO_CONTENT), Ok(()));
        assert_eq!(deleted(StatusCode::NOT_FOUND), Ok(()));
        assert!(
            deleted(StatusCode::SERVICE_UNAVAILABLE)
                .is_err()
        );
        assert!(deleted(StatusCode::FORBIDDEN).is_err());
    }
}
