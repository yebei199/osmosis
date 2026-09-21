//! 持久播放队列:队列定义、待应用的播放意图、播放端最近确认的执行状态。
//!
//! 三张表对着 `docs/adr/0031` 的三层裁决,**分开**而不是挤进一张:混在一起
//! 迟早会有人拿「数据库里写了」当「音箱已经在响」,而那正是这套模型要防的错。
//!
//! 与 [`crate::store::playlist`] 的分界:歌单是用户攒的一份**集合**,同一首歌
//! 只能有一条;队列是一次播放的**有序批次**,同一首歌可以出现多次,所以条目的
//! 身份是 `entry_id` 而不是 `(平台, 曲目)`。
//!
//! 与 [`crate::store::cache`] 的分界:那边删了只是慢一次,这边删了丢的是用户
//! 自己排的顺序。所以队列条目自带展示信息快照,**不外键指向 `platform_tracks`**。
//!
//! 每个函数都收 `account_id` 并把它写进归属检查:分成两步就总有一天会漏掉第一步。

use std::collections::HashMap;

use sqlx::{PgConnection, Postgres, Transaction};

use crate::error::AppError;

/// 写队列的函数收的是**事务**,不是连接。
///
/// 这一条是类型上的门,不是约定(#109 F-R1)。这个模块里每个写函数都跑好几条
/// SQL,而池里那条连接是自动提交的 —— 每条语句各自一个事务,于是:
///
/// - 中途失败留下半份数据(建队列写了头、没写条目);
/// - `SELECT ... FOR UPDATE` 拿的行锁在那条 SELECT 返回时就还回去了,
///   后面几步全程无保护,而注释里写着「两次并发发布必须排队」。
///
/// 收 `&mut PgConnection` 的话,这两种错都得靠调用方记得开事务,而忘了开
/// **没有任何症状**:测试照样绿,并发照样偶尔对。收这个类型之后,忘了开是
/// 一个编译错误。
///
/// 读函数不收它:单条 SELECT 本来就是原子的,多要一个事务只是噪音。
pub type Tx<'c> = Transaction<'c, Postgres>;

/// 回收旧版本时至少留几版。
///
/// 留的是「可能有人正在分页读」的那几版:一次读固定在一个 revision 上,而读到
/// 一半的那一方不会来打招呼。被播放端采用的版本另有保护,不靠这个数
// ponytail: 三版是拍的。真出现"读到一半版本没了",要的是读方带上租约,不是把它调大
const KEEP_REVISIONS: i64 = 3;

/// 上传一条队列条目时给的东西。
///
/// 展示信息跟着条目走,不向缓存要:曲目会下架、平台会故障、权限会失去,
/// 那时要保留的是条目身份、位置与**最后已知**的展示信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInput {
    pub platform: String,
    pub track_id: String,
    pub title: String,
    pub alias: Option<String>,
    pub artists: Vec<String>,
    pub cover: Option<String>,
    pub duration_ms: i64,
}

/// 队列里的一条,连同它在这一版里的身份与位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 同一首歌重复出现时,区分的是哪一次出现。跨版本稳定。
    pub entry_id: i64,
    /// 展示原序。播放顺序是执行状态,不在这里。
    pub position: i64,
    pub platform: String,
    pub track_id: String,
    pub title: String,
    pub alias: Option<String>,
    pub artists: Vec<String>,
    pub cover: Option<String>,
    pub duration_ms: i64,
}

/// 一次发布的产物:这是哪个队列的哪一版。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueRef {
    pub queue_id: i64,
    pub revision: i64,
}

/// 第二层:待应用的播放意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub device_id: String,
    pub revision: i64,
    pub entry_id: i64,
    /// 由发起方生成。重试同一次操作不该再次重置播放,服务端只比对它。
    pub operation_id: String,
    /// `pending` / `applied` / `failed`。
    pub state: String,
    pub reason: Option<String>,
}

/// 第三层:播放端最近确认的执行状态。**是一份报告,不是实时真相。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub device_id: String,
    /// 播放端进程启动时的毫秒挂钟,重启换一个。
    pub epoch: i64,
    pub state_seq: i64,
    /// 播放端**实际应用**的版本,与队列的最新版本分开。
    pub applied_revision: i64,
    pub entry_id: Option<i64>,
    /// 实际播放次序:`entry_id` 的排列。显式存,不让两端凭 seed 猜。
    ///
    /// `None` 是「这一条不带排列,沿用库里那份」。每秒那条上报走的就是
    /// `None` —— 带上的话每秒要重写五千个 bigint,而排列一个小时也不变一次。
    pub play_order: Option<Vec<i64>>,
    /// 列表循环的轮次。随机每轮重洗,排列与轮次要一起看。
    pub round: i64,
    pub position_ms: i64,
    pub play_state: String,
}

/// 一页条目,连同这一版一共有多少条。
///
/// `total` 与条目一起回而不是另开一条查询:调用方每次都要它(不然不知道
/// 还要不要翻下一页),而它本来就要为「这一版还在不在」数一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub total: i64,
    pub entries: Vec<Entry>,
}

/// 一个队列此刻的概况:最新版本、条目数,以及另外两层各自的最新一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueHead {
    pub queue_id: i64,
    pub revision: i64,
    pub total: i64,
    pub intent: Option<Intent>,
    pub report: Option<Report>,
}

/// 一条条目行的原样形状。起个名字是因为 clippy 数得出它有九列 ——
/// 而这九列就是 `play_queue_entries` 的列,不该为了好看拆成两个结构。
type EntryRow = (
    i64,
    i64,
    String,
    String,
    String,
    Option<String>,
    Vec<String>,
    Option<String>,
    i64,
);

/// 一条报告行的原样形状,理由同 [`EntryRow`]。
type ReportRow = (
    String,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<Vec<i64>>,
    i64,
    i64,
    String,
);

/// 新建一个队列,条目成为它的第一版。
pub async fn create(
    tx: &mut Tx<'_>,
    account_id: i64,
    device_id: &str,
    entries: &[EntryInput],
) -> Result<QueueRef, AppError> {
    check_size(entries)?;

    let (queue_id,): (i64,) = sqlx::query_as(
        "INSERT INTO play_queues
             (account_id, device_id, revision, next_entry_id)
         VALUES ($1, $2, 1, $3) RETURNING id",
    )
    .bind(account_id)
    .bind(device_id)
    .bind(entries.len() as i64 + 1)
    .fetch_one(&mut **tx)
    .await?;

    let entry_ids: Vec<i64> =
        (1..=entries.len() as i64).collect();
    insert_entries(tx, queue_id, 1, entries, &entry_ids)
        .await?;

    Ok(QueueRef {
        queue_id,
        revision: 1,
    })
}

/// 改一个队列:原子产生新版本,旧版本仍读得到。
///
/// `expected_revision` 对不上就整次拒绝 —— 两台设备同时改时,后到的那次不能
/// 凭「我也有一份完整列表」把别人的新版本盖掉。
pub async fn publish(
    tx: &mut Tx<'_>,
    account_id: i64,
    queue_id: i64,
    expected_revision: i64,
    entries: &[EntryInput],
) -> Result<QueueRef, AppError> {
    check_size(entries)?;

    // FOR UPDATE:两次并发发布必须排队,否则两边都读到同一个 revision,
    // 各自 +1 写进去,后写的那份把前一份的条目覆盖成同一个版本号。
    let (revision, mut next_entry_id): (i64, i64) =
        sqlx::query_as(
            "SELECT revision, next_entry_id FROM play_queues
             WHERE id = $1 AND account_id = $2 FOR UPDATE",
        )
        .bind(queue_id)
        .bind(account_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(AppError::NotFound)?;

    if revision != expected_revision {
        return Err(AppError::RevisionConflict);
    }

    let mut carried = occurrences(
        &previous_entries(tx, queue_id, revision).await?,
    );
    let entry_ids: Vec<i64> = entries
        .iter()
        .map(|entry| {
            let key = (
                entry.platform.clone(),
                entry.track_id.clone(),
            );
            carried
                .get_mut(&key)
                .and_then(|seen| {
                    if seen.is_empty() {
                        None
                    } else {
                        Some(seen.remove(0))
                    }
                })
                .unwrap_or_else(|| {
                    let fresh = next_entry_id;
                    next_entry_id += 1;
                    fresh
                })
        })
        .collect();

    let next = revision + 1;
    insert_entries(tx, queue_id, next, entries, &entry_ids)
        .await?;

    sqlx::query(
        "UPDATE play_queues
         SET revision = $2, next_entry_id = $3, updated_at = now()
         WHERE id = $1",
    )
    .bind(queue_id)
    .bind(next)
    .bind(next_entry_id)
    .execute(&mut **tx)
    .await?;

    reclaim(tx, queue_id, next).await?;

    Ok(QueueRef {
        queue_id,
        revision: next,
    })
}

/// 读一页。**固定在给定的 revision 上** —— 不会前半页旧顺序、后半页新顺序。
///
/// 版本已被回收(或压根不存在)时是 [`AppError::NotFound`],不是一页空的:
/// 空页与"这一版没了"在调用方那里是两种完全不同的处置。
pub async fn page(
    conn: &mut PgConnection,
    account_id: i64,
    queue_id: i64,
    revision: i64,
    offset: i64,
    limit: i64,
) -> Result<Page, AppError> {
    owned(conn, account_id, queue_id).await?;

    let (total,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM play_queue_entries
         WHERE queue_id = $1 AND revision = $2",
    )
    .bind(queue_id)
    .bind(revision)
    .fetch_one(&mut *conn)
    .await?;
    if total == 0 {
        return Err(AppError::NotFound);
    }

    let rows: Vec<EntryRow> = sqlx::query_as(
        "SELECT entry_id, position, platform, track_id,
                title, alias, artists, cover, duration_ms
         FROM play_queue_entries
         WHERE queue_id = $1 AND revision = $2
         ORDER BY position
         OFFSET $3 LIMIT $4",
    )
    .bind(queue_id)
    .bind(revision)
    .bind(offset.max(0))
    .bind(limit.clamp(1, contract::QUEUE_PAGE_LIMIT as i64))
    .fetch_all(conn)
    .await?;

    Ok(Page {
        total,
        entries: rows
            .into_iter()
            .map(
                |(
                    entry_id,
                    position,
                    platform,
                    track_id,
                    title,
                    alias,
                    artists,
                    cover,
                    duration_ms,
                )| Entry {
                    entry_id,
                    position,
                    platform,
                    track_id,
                    title,
                    alias,
                    artists,
                    cover,
                    duration_ms,
                },
            )
            .collect(),
    })
}

/// 队列此刻的概况:最新版本、条目数,以及意图与报告各自的最新一条。
pub async fn head(
    conn: &mut PgConnection,
    account_id: i64,
    queue_id: i64,
) -> Result<QueueHead, AppError> {
    let revision =
        owned(conn, account_id, queue_id).await?;

    let (total,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM play_queue_entries
         WHERE queue_id = $1 AND revision = $2",
    )
    .bind(queue_id)
    .bind(revision)
    .fetch_one(&mut *conn)
    .await?;

    let intent: Option<(
        String,
        i64,
        i64,
        String,
        String,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT device_id, revision, entry_id, operation_id, state, reason
         FROM play_queue_intents WHERE queue_id = $1",
    )
    .bind(queue_id)
    .fetch_optional(&mut *conn)
    .await?;

    let report: Option<ReportRow> = sqlx::query_as(
        "SELECT device_id, epoch, state_seq, applied_revision, entry_id,
                play_order, round, position_ms, play_state
         FROM play_queue_reports WHERE queue_id = $1",
    )
    .bind(queue_id)
    .fetch_optional(conn)
    .await?;

    Ok(QueueHead {
        queue_id,
        revision,
        total,
        intent: intent.map(
            |(
                device_id,
                revision,
                entry_id,
                operation_id,
                state,
                reason,
            )| Intent {
                device_id,
                revision,
                entry_id,
                operation_id,
                state,
                reason,
            },
        ),
        report: report.map(
            |(
                device_id,
                epoch,
                state_seq,
                applied_revision,
                entry_id,
                play_order,
                round,
                position_ms,
                play_state,
            )| Report {
                device_id,
                epoch,
                state_seq,
                applied_revision,
                entry_id,
                play_order,
                round,
                position_ms,
                play_state,
            },
        ),
    })
}

/// 写一条待应用的播放意图,覆盖上一条。
///
/// 一个队列同时只有一条:连点 A、B 时 B 覆盖 A,迟到的 A 因此再也应用不上 ——
/// 它带的 `operation_id` 已经不是当前这一条了。
pub async fn set_intent(
    tx: &mut Tx<'_>,
    account_id: i64,
    queue_id: i64,
    device_id: &str,
    revision: i64,
    entry_id: i64,
    operation_id: &str,
) -> Result<(), AppError> {
    owned(tx, account_id, queue_id).await?;

    sqlx::query(
        "INSERT INTO play_queue_intents
             (queue_id, device_id, revision, entry_id, operation_id, state)
         VALUES ($1, $2, $3, $4, $5, 'pending')
         ON CONFLICT (queue_id) DO UPDATE SET
             device_id = excluded.device_id,
             revision = excluded.revision,
             entry_id = excluded.entry_id,
             operation_id = excluded.operation_id,
             state = 'pending',
             reason = NULL,
             updated_at = now()",
    )
    .bind(queue_id)
    .bind(device_id)
    .bind(revision)
    .bind(entry_id)
    .bind(operation_id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// 给一条意图收尾:`applied` 或 `failed`。
///
/// `operation_id` 对不上就什么也不做并返回 `false` —— 那是一条已经被顶掉的
/// 旧操作在迟到汇报,收下它会把当前那条的状态改错。
pub async fn finish_intent(
    tx: &mut Tx<'_>,
    account_id: i64,
    queue_id: i64,
    operation_id: &str,
    applied: bool,
    reason: Option<&str>,
) -> Result<bool, AppError> {
    owned(tx, account_id, queue_id).await?;

    let done = sqlx::query(
        "UPDATE play_queue_intents
         SET state = $3, reason = $4, updated_at = now()
         WHERE queue_id = $1 AND operation_id = $2",
    )
    .bind(queue_id)
    .bind(operation_id)
    .bind(if applied { "applied" } else { "failed" })
    .bind(reason)
    .execute(&mut **tx)
    .await?;

    Ok(done.rows_affected() > 0)
}

/// 收下一份执行报告。旧 epoch 或倒退的序号一律拒,返回 `false`。
///
/// `(epoch, state_seq)` 是个可比的序:播放端重启换一个更大的 epoch,同一个
/// epoch 内序号递增。少了它,上一条连接上飘过来的残余报告会把新进程的状态盖掉,
/// 而症状是遥控器上的进度条倒退一次。
pub async fn record_report(
    tx: &mut Tx<'_>,
    account_id: i64,
    queue_id: i64,
    report: &Report,
) -> Result<bool, AppError> {
    owned(tx, account_id, queue_id).await?;

    let done = sqlx::query(
        "INSERT INTO play_queue_reports
             (queue_id, device_id, epoch, state_seq, applied_revision,
              entry_id, play_order, round, position_ms, play_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (queue_id) DO UPDATE SET
             device_id = excluded.device_id,
             epoch = excluded.epoch,
             state_seq = excluded.state_seq,
             applied_revision = excluded.applied_revision,
             entry_id = excluded.entry_id,
             play_order = COALESCE(
                 excluded.play_order, play_queue_reports.play_order),
             round = excluded.round,
             position_ms = excluded.position_ms,
             play_state = excluded.play_state,
             reported_at = now()
         WHERE (excluded.epoch, excluded.state_seq)
             > (play_queue_reports.epoch, play_queue_reports.state_seq)",
    )
    .bind(queue_id)
    .bind(&report.device_id)
    .bind(report.epoch)
    .bind(report.state_seq)
    .bind(report.applied_revision)
    .bind(report.entry_id)
    .bind(&report.play_order)
    .bind(report.round)
    .bind(report.position_ms)
    .bind(&report.play_state)
    .execute(&mut **tx)
    .await?;

    Ok(done.rows_affected() > 0)
}

/// 队列归不归这个账号,顺带给出它的最新版本。不归就是 [`AppError::NotFound`] ——
/// 回 403 等于确认了这个 id 存在。
async fn owned(
    conn: &mut PgConnection,
    account_id: i64,
    queue_id: i64,
) -> Result<i64, AppError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT revision FROM play_queues
         WHERE id = $1 AND account_id = $2",
    )
    .bind(queue_id)
    .bind(account_id)
    .fetch_optional(conn)
    .await?;

    row.map(|(revision,)| revision)
        .ok_or(AppError::NotFound)
}

/// 规模检查。空队列同样拒:一个没有条目的队列没有任何用处,
/// 而它会让「读不到条目」这件事多出一种无害的解释。
fn check_size(
    entries: &[EntryInput],
) -> Result<(), AppError> {
    if entries.is_empty() {
        return Err(AppError::Invalid("队列不能是空的"));
    }
    if entries.len() > contract::MAX_QUEUE_ENTRIES {
        return Err(AppError::QueueTooLarge);
    }
    Ok(())
}

/// 上一版的条目,按位置。只取身份 —— 沿用 `entry_id` 只需要它。
async fn previous_entries(
    conn: &mut PgConnection,
    queue_id: i64,
    revision: i64,
) -> Result<Vec<(String, String, i64)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT platform, track_id, entry_id
         FROM play_queue_entries
         WHERE queue_id = $1 AND revision = $2
         ORDER BY position",
    )
    .bind(queue_id)
    .bind(revision)
    .fetch_all(conn)
    .await?)
}

/// 把上一版按 `(平台, 曲目)` 归拢成「第几次出现用哪个 entry_id」。
///
/// 新版本里同一首歌的第 k 次出现,沿用旧版本第 k 次出现的号。重排与增删都能
/// 对上,而队列允许重复,所以不能只按曲目配一次。
// ponytail: 这是个启发式。真要精确跟踪某一条被挪到哪儿,得让客户端把 entry_id
// 一起传回来 —— 等到有人抱怨"换了个顺序之后正在放的那首丢了"再加
fn occurrences(
    previous: &[(String, String, i64)],
) -> HashMap<(String, String), Vec<i64>> {
    let mut seen: HashMap<(String, String), Vec<i64>> =
        HashMap::new();
    for (platform, track_id, entry_id) in previous {
        seen.entry((platform.clone(), track_id.clone()))
            .or_default()
            .push(*entry_id);
    }
    seen
}

/// 写一整版的条目。一条 `UNNEST` 而不是 N 条 INSERT:五千首就是五千次往返。
async fn insert_entries(
    conn: &mut PgConnection,
    queue_id: i64,
    revision: i64,
    entries: &[EntryInput],
    entry_ids: &[i64],
) -> Result<(), AppError> {
    let positions: Vec<i64> =
        (0..entries.len() as i64).collect();
    let platforms: Vec<&str> = entries
        .iter()
        .map(|entry| entry.platform.as_str())
        .collect();
    let track_ids: Vec<&str> = entries
        .iter()
        .map(|entry| entry.track_id.as_str())
        .collect();
    let titles: Vec<&str> = entries
        .iter()
        .map(|entry| entry.title.as_str())
        .collect();
    let aliases: Vec<Option<&str>> = entries
        .iter()
        .map(|entry| entry.alias.as_deref())
        .collect();
    let covers: Vec<Option<&str>> = entries
        .iter()
        .map(|entry| entry.cover.as_deref())
        .collect();
    let durations: Vec<i64> = entries
        .iter()
        .map(|entry| entry.duration_ms)
        .collect();
    // 歌手是 TEXT[],整批就是 TEXT[][] —— Postgres 的多维数组要求各行等长,
    // 所以这一列单独按行写,不进 UNNEST。
    let artists: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            serde_json::Value::from(entry.artists.clone())
        })
        .collect();

    sqlx::query(
        "INSERT INTO play_queue_entries
             (queue_id, revision, entry_id, position, platform, track_id,
              title, alias, artists, cover, duration_ms)
         SELECT $1, $2, u.entry_id, u.position, u.platform, u.track_id,
                u.title, u.alias,
                ARRAY(SELECT jsonb_array_elements_text(u.artists)),
                u.cover, u.duration_ms
         FROM UNNEST($3::bigint[], $4::bigint[], $5::text[], $6::text[],
                     $7::text[], $8::text[], $9::jsonb[], $10::text[],
                     $11::bigint[])
              AS u(entry_id, position, platform, track_id,
                   title, alias, artists, cover, duration_ms)",
    )
    .bind(queue_id)
    .bind(revision)
    .bind(entry_ids)
    .bind(&positions)
    .bind(&platforms)
    .bind(&track_ids)
    .bind(&titles)
    .bind(&aliases)
    .bind(&artists)
    .bind(&covers)
    .bind(&durations)
    .execute(conn)
    .await?;

    Ok(())
}

/// 回收旧版本的条目。
///
/// 两种版本**不能**回收:播放端已经采用的那一版(它此刻正照着它放),以及
/// 待应用意图指着的那一版。除此之外留最近 [`KEEP_REVISIONS`] 版,
/// 给正在分页读的那一方一点余量。
async fn reclaim(
    conn: &mut PgConnection,
    queue_id: i64,
    current: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM play_queue_entries e
         WHERE e.queue_id = $1
           AND e.revision <= $2 - $3
           AND e.revision <> COALESCE(
                 (SELECT applied_revision FROM play_queue_reports
                  WHERE queue_id = $1), -1)
           AND e.revision <> COALESCE(
                 (SELECT revision FROM play_queue_intents
                  WHERE queue_id = $1), -1)",
    )
    .bind(queue_id)
    .bind(current)
    .bind(KEEP_REVISIONS)
    .execute(conn)
    .await?;

    Ok(())
}
