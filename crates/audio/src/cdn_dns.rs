//! 让 CDN 主机的 DNS 一直是热的(#139)。
//!
//! #137 ⑥ 的开流分段计时里,桌面冷开流 500ms 级的离群全落在 `dns` 那段:系统
//! resolver 缓存没命中,要一路问到上游。直链只落在少数几台 CDN 主机上,所以
//! 启动后就在后台把它们解析一遍,并赶在 TTL 过期前再解析,让开流时那次
//! getaddrinfo 总能命中系统缓存。
//!
//! 只暖系统的缓存,不在进程里另存地址:过期与换 IP 仍由系统 resolver 按 TTL
//! 管,这里不必重新实现一遍。失败只记 debug —— 暖不上就是照旧冷开流。

use std::future::Future;
use std::time::Duration;

/// 网易云直链落的 CDN 主机。来源:#137 ⑥ 开流计时日志里 `stream:` 行的
/// `host=` 字段,2026-09-25 实测只出现过这四台。只写主机名,不写 IP ——
/// IP 由 DNS 轮转,写死就是在替 CDN 做调度。
const CDN_HOSTS: [&str; 4] = [
    "m7.music.126.net",
    "m8.music.126.net",
    "m701.music.126.net",
    "m801.music.126.net",
];

/// 多久重新解析一次。这四台的 TTL 实测 600 秒(2026-09-25 `dig`),
/// 取一半,系统缓存过期之前总有一次新的解析垫上。
const REFRESH: Duration = Duration::from_secs(300);

/// 在后台开始暖 [`CDN_HOSTS`]。立刻返回,不阻塞启动。
pub fn warm_cdn_dns() {
    crate::runtime::runtime().spawn(async {
        loop {
            warm(&CDN_HOSTS, lookup).await;
            tokio::time::sleep(REFRESH).await;
        }
    });
}

async fn lookup(host: &'static str) -> std::io::Result<()> {
    tokio::net::lookup_host((host, 443)).await.map(drop)
}

/// 把每台主机解析一遍。一台失败不耽误其余几台。
async fn warm<F, Fut>(hosts: &[&'static str], resolve: F)
where
    F: Fn(&'static str) -> Fut,
    Fut: Future<Output = std::io::Result<()>>,
{
    for &host in hosts {
        if let Err(error) = resolve(host).await {
            log::debug!("预解析 {host} 没成: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::warm;

    /// 中间那台解析失败,后面的照样解析 —— 断网或某台下线时,
    /// 其余几台仍然该是热的。
    #[tokio::test]
    async fn a_failed_host_does_not_stop_the_rest() {
        let asked = RefCell::new(Vec::new());
        warm(&["a", "b", "c"], |host| {
            asked.borrow_mut().push(host);
            async move {
                if host == "b" {
                    Err(std::io::Error::other("解析失败"))
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert_eq!(*asked.borrow(), ["a", "b", "c"]);
    }
}
