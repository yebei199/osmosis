//! 持久播放队列的六条路由(`docs/adr/0031`)。
//!
//! 曲目数据走 HTTP 而不是信令:它的体积随用户的歌单长度增长,而信令通道是按
//! SDP 与 ICE 设计的,64 KiB 一撞就是整条连接断掉(见 `contract::MAX_SIGNAL_BYTES`
//! 与 #109)。WebSocket 那边只留小消息与唤醒通知。
//!
//! 三层裁决各有各的入口,不合成一条「更新队列」:
//!
//! | 层 | 路由 | 谁来调 |
//! |---|---|---|
//! | 队列定义 | `POST /queues`、`POST /queues/{id}/revisions`、`GET /queues/{id}` | 点播的那一端 |
//! | 待应用的播放意图 | `POST /queues/{id}/intent` | 点播的那一端 |
//! | 实际执行状态 | `POST /queues/{id}/report` | 播放端 |
//! | (三层一起看) | `GET /queues/{id}/head` | 两端恢复时对账 |
//!
//! 这一层只管 HTTP 的形状,归属与并发裁决在 `server::store::queue`。

#[cfg(test)]
mod tests;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use contract::{
    CreateQueueDto, PublishQueueDto, QueueEntryDto,
    QueueHeadDto, QueueIntentDto, QueueIntentState,
    QueuePageDto, QueueRefDto, QueueReportAckDto,
    QueueReportDto, RemotePlayState, SetQueueIntentDto,
    TrackDto,
};
use serde::Deserialize;

use server::error;
use server::error::Failure;
use server::store::account::Account;
use server::store::queue::{
    self, Entry, EntryInput, Intent, QueueRef, Report,
};

use crate::{AppState, conn};

/// `GET /queues/{id}` 的查询参数。
///
/// `revision` 必给,不默认成「最新那一版」:读的人手上有一个确定的版本号,
/// 拿不到它就该去问 `head`。默认成最新的话,分页读一份长队列会前半页旧顺序、
/// 后半页新顺序,而那种错在界面上看起来只是"有几首歌重复了"。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PageQuery {
    pub(crate) revision: i64,
    pub(crate) offset: Option<i64>,
    pub(crate) limit: Option<i64>,
}

/// `POST /queues` —— 把用户点下那一刻看到的有序条目冻结成第一版。
///
/// 冻结的是**条目**,不是一次查询:查询时刻、缓存刷新、排序都会改变结果,
/// 拿来源描述符当队列,刷新一次正在放的东西就可能被改掉(`docs/adr/0031`)。
pub(crate) async fn create_queue(
    State(state): State<AppState>,
    account: Account,
    Json(body): Json<CreateQueueDto>,
) -> Result<Json<QueueRefDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let entries = inputs(&body.tracks);

    queue::create(
        &mut conn,
        account.id,
        &body.device_id,
        &entries,
    )
    .await
    .map(as_ref)
    .map_err(|err| error::map_error(&err))
}

/// `POST /queues/{id}/revisions` —— 改一个队列,原子产生新版本。
///
/// 整份新内容而不是增量:第一版不做任意增量编辑协议,那需要一套
/// `base_revision` 加丢失回退,而队列编辑到底频不频繁还没测过
/// (`docs/adr/0031` 三)。
pub(crate) async fn publish_queue(
    State(state): State<AppState>,
    account: Account,
    Path(queue_id): Path<i64>,
    Json(body): Json<PublishQueueDto>,
) -> Result<Json<QueueRefDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let entries = inputs(&body.tracks);

    queue::publish(
        &mut conn,
        account.id,
        queue_id,
        body.expected_revision,
        &entries,
    )
    .await
    .map(as_ref)
    .map_err(|err| error::map_error(&err))
}

/// `GET /queues/{id}?revision=&offset=&limit=` —— 读一页,固定在给定版本上。
pub(crate) async fn queue_page(
    State(state): State<AppState>,
    account: Account,
    Path(queue_id): Path<i64>,
    Query(query): Query<PageQuery>,
) -> Result<Json<QueuePageDto>, Failure> {
    let mut conn = conn(&state.pool).await?;
    let offset = query.offset.unwrap_or(0).max(0);
    // 不给就给满一页。夹在上限内那一步在 store 里 —— 两处各夹一次的话,
    // 迟早只改了一处。
    let limit = query
        .limit
        .unwrap_or(contract::QUEUE_PAGE_LIMIT as i64);

    let page = queue::page(
        &mut conn,
        account.id,
        queue_id,
        query.revision,
        offset,
        limit,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(Json(QueuePageDto {
        queue_id,
        revision: query.revision,
        total: page.total,
        offset,
        entries: page
            .entries
            .into_iter()
            .map(as_entry)
            .collect(),
    }))
}

/// `GET /queues/{id}/head` —— 三层各自的最新一条。
///
/// 恢复时两端都读它:播放端拿它对账(HTTP 已提交而 WebSocket 通知丢了的那条
/// 变更就在 `intent` 里),遥控器拿它知道该去取哪一版。
pub(crate) async fn queue_head(
    State(state): State<AppState>,
    account: Account,
    Path(queue_id): Path<i64>,
) -> Result<Json<QueueHeadDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let head = queue::head(&mut conn, account.id, queue_id)
        .await
        .map_err(|err| error::map_error(&err))?;

    Ok(Json(QueueHeadDto {
        queue_id: head.queue_id,
        revision: head.revision,
        total: head.total,
        intent: head.intent.map(as_intent),
        report: head.report.map(as_report),
    }))
}

/// `POST /queues/{id}/intent` —— 记下「请播这一条」。
///
/// 与真的开始播分开:意图落库只说明这一下没丢,**不说明音箱在响**。
/// 播放端确认之前,界面该标「新版本待应用」(`docs/adr/0031` 一)。
pub(crate) async fn set_queue_intent(
    State(state): State<AppState>,
    account: Account,
    Path(queue_id): Path<i64>,
    Json(body): Json<SetQueueIntentDto>,
) -> Result<StatusCode, Failure> {
    let mut conn = conn(&state.pool).await?;

    queue::set_intent(
        &mut conn,
        account.id,
        queue_id,
        &body.device_id,
        body.revision,
        body.entry_id,
        &body.operation_id,
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    Ok(StatusCode::NO_CONTENT)
}

/// `POST /queues/{id}/report` —— 播放端说「我到哪了」,顺带说那次操作成没成。
///
/// 两件事同一条请求:播放端知道它们是同一刻的事,分两次发就会出现两者互相
/// 矛盾的中间态,而界面正是照着这两样决定还挂不挂「待应用」。
///
/// 迟到的报告回 `accepted: false` 而不是 4xx:乱序是这条链路的常态,不是故障。
/// 回错误的话客户端要么重试(更乱)、要么打一条看起来像故障的日志,
/// 而它唯一该做的事是继续报下一条。
pub(crate) async fn report_queue_state(
    State(state): State<AppState>,
    account: Account,
    Path(queue_id): Path<i64>,
    Json(body): Json<QueueReportDto>,
) -> Result<Json<QueueReportAckDto>, Failure> {
    let mut conn = conn(&state.pool).await?;

    let accepted = queue::record_report(
        &mut conn,
        account.id,
        queue_id,
        &Report {
            device_id: body.device_id,
            epoch: body.epoch,
            state_seq: body.state_seq,
            applied_revision: body.applied_revision,
            entry_id: body.entry_id,
            play_order: body.play_order,
            round: body.round,
            position_ms: body.position_ms,
            play_state: play_state_name(body.state)
                .to_owned(),
        },
    )
    .await
    .map_err(|err| error::map_error(&err))?;

    // 只有收下的报告才给意图收尾。被判过期的那一条来自旧 epoch 或乱序,
    // 让它去改当前意图的状态,等于让一条已经不作数的汇报改写现在的事实。
    if accepted && let Some(outcome) = body.operation {
        queue::finish_intent(
            &mut conn,
            account.id,
            queue_id,
            &outcome.operation_id,
            outcome.applied,
            outcome.reason.as_deref(),
        )
        .await
        .map_err(|err| error::map_error(&err))?;
    }

    Ok(Json(QueueReportAckDto { accepted }))
}

/// 上传的 [`TrackDto`] 翻成条目输入。
///
/// 展示字段抄进条目自己的快照,**不向 `platform_tracks` 要**:曲目会下架、
/// 平台会故障,那时要保留的是条目身份、位置与最后已知的展示信息
/// (`docs/adr/0031` 五)。
fn inputs(tracks: &[TrackDto]) -> Vec<EntryInput> {
    tracks
        .iter()
        .map(|track| EntryInput {
            platform: track.platform.clone(),
            track_id: track.id.clone(),
            title: track.title.clone(),
            alias: track.alias.clone(),
            artists: track.artists.clone(),
            cover: track.cover.clone(),
            duration_ms: track.duration_ms,
        })
        .collect()
}

fn as_ref(published: QueueRef) -> Json<QueueRefDto> {
    Json(QueueRefDto {
        queue_id: published.queue_id,
        revision: published.revision,
    })
}

fn as_entry(entry: Entry) -> QueueEntryDto {
    QueueEntryDto {
        entry_id: entry.entry_id,
        position: entry.position,
        track: TrackDto {
            platform: entry.platform,
            id: entry.track_id,
            title: entry.title,
            alias: entry.alias,
            artists: entry.artists,
            cover: entry.cover,
            duration_ms: entry.duration_ms,
        },
    }
}

fn as_intent(intent: Intent) -> QueueIntentDto {
    QueueIntentDto {
        device_id: intent.device_id,
        revision: intent.revision,
        entry_id: intent.entry_id,
        operation_id: intent.operation_id,
        state: intent_state(&intent.state),
        reason: intent.reason,
    }
}

fn as_report(report: Report) -> QueueReportDto {
    QueueReportDto {
        device_id: report.device_id,
        epoch: report.epoch,
        state_seq: report.state_seq,
        applied_revision: report.applied_revision,
        entry_id: report.entry_id,
        play_order: report.play_order,
        round: report.round,
        position_ms: report.position_ms,
        state: play_state(&report.play_state),
        // 操作下场不回给读的人:它是写那一刻的一次性汇报,
        // 已经落在 `intent.state` 上了。
        operation: None,
    }
}

/// 库里那个字符串翻回枚举。认不出就当空闲 —— 库里存的是本服务自己写进去的,
/// 认不出说明写它的那一版与读它的这一版对不上,那时把它当成"在放"更糟。
fn play_state(name: &str) -> RemotePlayState {
    match name {
        "buffering" => RemotePlayState::Buffering,
        "playing" => RemotePlayState::Playing,
        "paused" => RemotePlayState::Paused,
        _ => RemotePlayState::Idle,
    }
}

const fn play_state_name(
    state: RemotePlayState,
) -> &'static str {
    match state {
        RemotePlayState::Idle => "idle",
        RemotePlayState::Buffering => "buffering",
        RemotePlayState::Playing => "playing",
        RemotePlayState::Paused => "paused",
    }
}

/// 同理。认不出当 `pending`:那一档最保守 —— 界面会继续挂「待应用」,
/// 而不是谎称已经在放了。
fn intent_state(name: &str) -> QueueIntentState {
    match name {
        "applied" => QueueIntentState::Applied,
        "failed" => QueueIntentState::Failed,
        _ => QueueIntentState::Pending,
    }
}
