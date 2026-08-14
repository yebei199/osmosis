//! 本 crate 私有的 tokio 运行时,取流与播放器共用一个。

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// 后台多线程 tokio runtime,专门跑下载。
///
/// 与 `api` 里那个同构、同理由(`docs/adr/0002`),但**必须是另一个** ——
/// 两个 crate 谁也不依赖谁。
///
/// 多线程是硬要求,不是性能选择:[`load`] 里解码器要**阻塞读**这条流,
/// 而喂它的下载任务跑在同一个 runtime 上。单线程 runtime 里两者互等,
/// 症状是整个调用永久挂起、没有任何报错。
pub(crate) fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Runtime::new()
            .expect("failed to start tokio runtime")
    })
}
