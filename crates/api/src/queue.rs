//! 持久播放队列的客户端(`docs/adr/0031`)。
//!
//! 曲目数据走这里而不是信令:它的体积随用户的歌单长度增长,而信令那条
//! 64 KiB 一撞就是整条连接断掉。WebSocket 上只剩小消息与唤醒通知。

use contract::{
    CreateQueueDto, PublishQueueDto, QueueEntryDto,
    QueueHeadDto, QueuePageDto, QueueRefDto,
    QueueReportAckDto, QueueReportDto, SetQueueIntentDto,
    TrackDto,
};

use crate::url::{
    queue_head_url, queue_intent_url, queue_page_url,
    queue_report_url, queue_revisions_url, queues_url,
};
use crate::{ApiError, platform};

/// `POST /queues` —— 把用户点下那一刻看到的有序条目冻结成第一版。
pub async fn create_queue(
    device_id: &str,
    tracks: Vec<TrackDto>,
) -> Result<QueueRefDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        queues_url(),
        Some(CreateQueueDto {
            device_id: device_id.to_owned(),
            tracks,
        }),
    )
    .await
}

/// `POST /queues/{id}/revisions` —— 改队列,原子产生新版本。
pub async fn publish_queue(
    queue_id: i64,
    expected_revision: i64,
    tracks: Vec<TrackDto>,
) -> Result<QueueRefDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        queue_revisions_url(queue_id),
        Some(PublishQueueDto {
            expected_revision,
            tracks,
        }),
    )
    .await
}

/// `GET /queues/{id}?revision=&offset=&limit=` —— 读一页。
pub async fn queue_page(
    queue_id: i64,
    revision: i64,
    offset: i64,
    limit: i64,
) -> Result<QueuePageDto, ApiError> {
    platform::get_json(queue_page_url(
        queue_id, revision, offset, limit,
    ))
    .await
}

/// `GET /queues/{id}/head` —— 三层各自的最新一条。
pub async fn queue_head(
    queue_id: i64,
) -> Result<QueueHeadDto, ApiError> {
    platform::get_json(queue_head_url(queue_id)).await
}

/// `POST /queues/{id}/intent` —— 记下「请播这一条」。
pub async fn set_queue_intent(
    queue_id: i64,
    intent: SetQueueIntentDto,
) -> Result<(), ApiError> {
    platform::send_no_content(
        reqwest::Method::POST,
        queue_intent_url(queue_id),
        Some(intent),
    )
    .await
}

/// `POST /queues/{id}/report` —— 报执行状态,顺带报操作下场。
pub async fn report_queue_state(
    queue_id: i64,
    report: QueueReportDto,
) -> Result<QueueReportAckDto, ApiError> {
    platform::send_json(
        reqwest::Method::POST,
        queue_report_url(queue_id),
        Some(report),
    )
    .await
}

/// 把一个版本的条目**整份**取下来。
///
/// 全部到齐才返回:执行副本要原子替换,半份列表不能拿去放
/// (`docs/adr/0031` 七)。中途任何一页失败,整次失败,调用方保留旧副本。
pub async fn fetch_queue(
    queue_id: i64,
    revision: i64,
) -> Result<Vec<QueueEntryDto>, ApiError> {
    collect_pages(|offset| async move {
        queue_page(
            queue_id,
            revision,
            offset,
            contract::QUEUE_PAGE_LIMIT as i64,
        )
        .await
    })
    .await
}

/// 分页拼装。抽出来单独可测 —— 它的三种坏情形都不会在正常联调里出现,
/// 而每一种都会让执行端拿到一份错的队列。
async fn collect_pages<F, Fut>(
    mut fetch: F,
) -> Result<Vec<QueueEntryDto>, ApiError>
where
    F: FnMut(i64) -> Fut,
    Fut: Future<Output = Result<QueuePageDto, ApiError>>,
{
    let mut entries: Vec<QueueEntryDto> = Vec::new();

    loop {
        let page = fetch(entries.len() as i64).await?;

        // 读的时候版本被换掉了。固定版本的读取本来就不该发生这种事,
        // 真发生了说明服务端没按 `revision` 钉住 —— 拼出来的会是
        // 前半页旧顺序、后半页新顺序,而那在界面上只表现为「有几首歌重复了」。
        if entries.is_empty() {
            entries.reserve(page.total.max(0) as usize);
        }

        let done = page.total as usize;
        if page.entries.is_empty() {
            // 还没取够却给了空页:再要下去就是死循环。宁可整次失败 ——
            // 半份列表拿去执行,用户听到的是一个他没点过的队列。
            if entries.len() < done {
                return Err(ApiError::Decode(format!(
                    "队列 {} 只取到 {} 条,服务端说共 {} 条",
                    page.queue_id,
                    entries.len(),
                    page.total
                )));
            }
            return Ok(entries);
        }

        entries.extend(page.entries);
        if entries.len() >= done {
            return Ok(entries);
        }
    }
}

#[cfg(test)]
mod tests;
