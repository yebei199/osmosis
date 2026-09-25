//! 上游直链的短期缓存(#139)。
//!
//! `/play` 往返是起播单段最大的一块,而同一首在几分钟内再点很常见。网易云的
//! 直链带签名、会过期,但它自己说了还能用多久(`expires_in_seconds`)——只要
//! 剩下的时间够客户端用这一条链从头放到尾,就不必再问上游。
//!
//! 「够不够」按整首算,不是按「此刻没过期」算:客户端整首歌用同一条链断点续传,
//! 放到一半链接失效就是半路断声。所以一条链能交出去的最后时刻是
//! 「签发时刻 + 有效期 − 整首时长 − [`MARGIN`]」,记成 `usable_until`。
//! 上游没给有效期或时长,就算不出这个时刻,不缓存。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use contract::PlaySourceDto;

use server::bangdream::proto::PlaySource;

#[cfg(test)]
mod tests;

/// 整首时长之外再留的余量:暂停、拖回去重听、网络慢,都会让一首歌
/// 放完的那一刻晚于「开始 + 时长」。
const MARGIN: Duration = Duration::from_secs(120);

/// 最多记几条。一条是一个 URL 加几个字段,几百字节;按一人一天几百首算
/// 这够几个人用,满了就先扔最早不能用的那条。
const CAPACITY: usize = 1024;

/// 账号 + 曲目 + 档位。直链带着账号的签名与权限,不能跨账号给。
pub(crate) type LinkKey = (i64, String, i32);

struct Entry {
    usable_until: Instant,
    source: PlaySourceDto,
}

/// 每个 [`LinkKey`] 最近一次从上游拿到的直链。只在内存里:重启丢了
/// 也只是多问一次上游。
#[derive(Clone, Default)]
pub(crate) struct SignedLinks(
    Arc<Mutex<HashMap<LinkKey, Entry>>>,
);

impl SignedLinks {
    fn lock(
        &self,
    ) -> MutexGuard<'_, HashMap<LinkKey, Entry>> {
        // 锁里只有 HashMap 的读写,不会在持锁时 panic;真毒化了也照用
        self.0.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// `now` 时刻还能交出去的那条,没有或不够放完一整首就是 `None`。
    pub(crate) fn get(
        &self,
        key: &LinkKey,
        now: Instant,
    ) -> Option<PlaySourceDto> {
        self.lock()
            .get(key)
            .filter(|entry| now < entry.usable_until)
            .map(|entry| entry.source.clone())
    }

    /// 记下 `issued_at` 那一刻问上游拿到的源。
    ///
    /// `issued_at` 取**发请求之前**的时刻:上游签发只会更晚,按早的算只会少用
    /// 一会儿,不会多用。
    pub(crate) fn record(
        &self,
        key: LinkKey,
        upstream: &PlaySource,
        source: PlaySourceDto,
        issued_at: Instant,
    ) {
        let Some(usable_until) =
            usable_until(upstream, issued_at)
        else {
            return;
        };

        let mut links = self.lock();
        if links.len() >= CAPACITY
            && !links.contains_key(&key)
        {
            // ponytail: 满了才线性扫一遍找最早不能用的,上千条仍是微秒级;
            // 容量要上万再换成按时刻排序的结构
            let stale = links
                .iter()
                .min_by_key(|(_, entry)| entry.usable_until)
                .map(|(stale, _)| stale.clone());
            if let Some(stale) = stale {
                links.remove(&stale);
            }
        }
        links.insert(
            key,
            Entry {
                usable_until,
                source,
            },
        );
    }
}

/// 这条链交出去的最后时刻;上游没给有效期或时长、或有效期本就不够放完整首,
/// 都是 `None`。
fn usable_until(
    upstream: &PlaySource,
    issued_at: Instant,
) -> Option<Instant> {
    let expires_in =
        u64::try_from(upstream.expires_in_seconds)
            .ok()
            .filter(|&secs| secs > 0)?;
    let duration = u64::try_from(upstream.duration_ms)
        .ok()
        .filter(|&ms| ms > 0)?;

    issued_at
        .checked_add(Duration::from_secs(expires_in))?
        .checked_sub(
            Duration::from_millis(duration) + MARGIN,
        )
        .filter(|&until| until > issued_at)
}
