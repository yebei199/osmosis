//! API 客户端:把"取一份服务端健康状态"这样的意图,翻译成一次具体的网络往返。
//!
//! 这里是各端运行时能力差异(有无线程、能否阻塞)被吸收的地方,差异到此为止,
//! 不再向上传播。对外暴露的 `async fn` 在 native 与 wasm 上**签名完全相同**;
//! `Send` 约束只存在于本 crate 内部的 `platform` 模块里。见 `docs/adr/0002`。

use contract::{HealthDto, PROTOCOL_VERSION};

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

/// 唯一按 target 分叉的地方。两个实现的**签名相同**,差异不外泄。
#[cfg(not(target_arch = "wasm32"))]
mod platform {
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

    pub(super) async fn get_json<
        T: DeserializeOwned + Send + 'static,
    >(
        url: String,
    ) -> Result<T, ApiError> {
        // spawn 把请求丢到后台线程池;await 的是 JoinHandle,它可以在任意
        // 线程上被 poll —— 包括 slint 的 UI 线程。
        runtime()
            .spawn(async move {
                let response = reqwest::get(url)
                    .await
                    .map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })?
                    .error_for_status()
                    .map_err(|e| {
                        ApiError::Transport(e.to_string())
                    })?;
                response.json::<T>().await.map_err(|e| {
                    ApiError::Decode(e.to_string())
                })
            })
            .await
            .map_err(|join_error| {
                ApiError::Transport(join_error.to_string())
            })?
    }
}

/// wasm 上没有线程,请求由浏览器的 fetch 驱动;future 不是 `Send`,无所谓。
#[cfg(target_arch = "wasm32")]
mod platform {
    use serde::de::DeserializeOwned;

    use super::ApiError;

    pub(super) async fn get_json<T: DeserializeOwned>(
        url: String,
    ) -> Result<T, ApiError> {
        let response = reqwest::get(url)
            .await
            .map_err(|e| ApiError::Transport(e.to_string()))?
            .error_for_status()
            .map_err(|e| {
                ApiError::Transport(e.to_string())
            })?;
        response
            .json::<T>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))
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
    fn 版本一致时原样返回_dto() {
        let ok = check_version(dto(PROTOCOL_VERSION))
            .expect("版本一致时不应报错");
        assert_eq!(ok.protocol_version, PROTOCOL_VERSION);
        assert_eq!(ok.status, "ok");
    }

    /// 客户端旧、服务端新:必须报错而不是接受。
    #[test]
    fn 服务端版本更高时报_版本不匹配() {
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
    fn 服务端版本更低时报_版本不匹配() {
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
    fn 服务端版本为零时报_版本不匹配() {
        let err =
            check_version(dto(0)).expect_err("版本为 0 时应报错");
        assert!(matches!(
            err,
            ApiError::VersionMismatch { actual: 0, .. }
        ));
    }

    /// 钉住会显示给用户的那句文案。措辞一改这里就红 —— 这是特性:
    /// 文案里的每个汉字都必须在 `crates/ui/fonts/cjk-subset.ttf` 里,
    /// 改了措辞就得重跑 `just font-subset`,否则 web 端显示成豆腐块。
    #[test]
    fn 版本不匹配的文案含双方版本号() {
        let err = ApiError::VersionMismatch {
            expected: 1,
            actual: 2,
        };
        assert_eq!(
            err.to_string(),
            "协议版本不匹配: 本机 v1,服务端 v2"
        );
    }
}
