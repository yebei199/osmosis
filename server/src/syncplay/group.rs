//! 组的全局播放状态:接线(#142)。
//!
//! 每个账号至多一个组,状态只在服务端,落在 `play_groups` 那一行。每一条意图都是同一个
//! 形状:开事务、锁这一行、把放完的先往下推、应用意图、掉线规则、版本加一、写回、
//! 提交,然后经名册把新状态推给账号下每台在线设备。规则本身在 [`timeline`],这里只管
//! 读、锁、写、推。
//!
//! 另外三处也会改状态:设备出册([`device_left`],最后一台出声设备掉线就暂停)、放完
//! 自动续播([`spawn_roller`])、设备入册时推一份当前状态([`greet`])。

pub mod timeline;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use contract::{
    GroupNowDto, GroupPickDto, GroupStateDto, NextEntryDto,
    ServerSignal, TrackDto, TransportOpDto,
};
use sqlx::{PgConnection, PgPool};

use crate::error::AppError;
use crate::store::group as rows;
use crate::store::queue::{self, Entry, EntryInput};
use crate::store::{blocks, facets};
use crate::syncplay::clock;
use crate::syncplay::signaling::{AccountId, SharedRoster};
use timeline::{Group, Playlist, Refusal};

/// 组队列归这台「设备」。组里的点歌都发到它名下那一个队列的新版本上,与各台设备自己
/// 独奏时的队列分开 —— 否则某台退出组后独奏发布的新版本,会把组还在放的那一版挤掉。
pub const GROUP_QUEUE_DEVICE: &str = "group";

/// 兜底续播多久看一次。平常是出声设备真正放完时报上来推进([`advance`]),这一拍只接
/// 没人报的那种(都卡住了、报丢了),晚几百毫秒不要紧。
const ROLL_EVERY: Duration = Duration::from_millis(500);

/// 在放的组多久记一次「服务端还活着」(见 `store::group::beat`)。服务端挂掉时暂停的
/// 位置就差在这一拍之内。
const BEAT_EVERY: u32 = 4;

/// 一条意图。
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Play(GroupPickDto),
    Transport(TransportOpDto),
    /// 改在这几台出声;`seed` 是组还没有歌时,发起那台本机正在放的那一份。
    Outputs {
        outputs: Vec<String>,
        seed: Option<contract::GroupSeedDto>,
    },
    Leave,
    /// 往组此刻那一版(`queue`)的队尾续几首,不换在放的那一首(电台续歌,#165)。
    Append {
        queue: (i64, i64),
        tracks: Vec<TrackDto>,
    },
}

/// 挂钟微秒。时间线落库用它:服务端单调钟每次启动从零起,重启后换算不回来。
pub fn wall_now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_micros() as i64)
}

pub async fn apply(
    pool: &PgPool,
    roster: &SharedRoster,
    account: AccountId,
    device: &str,
    intent: Intent,
) -> Result<Option<GroupStateDto>, AppError> {
    let mut tx = pool.begin().await?;
    let mut group = rows::lock(&mut tx, account).await?;
    let at = wall_now_us();
    let mut entries =
        current_entries(&mut tx, account, &mut group)
            .await?;
    let before = started(&group);
    group.roll(
        &blocked_playlist(&mut tx, account, &entries)
            .await?,
        at,
    );

    match intent {
        Intent::Play(pick) => {
            if !group.includes(device) {
                return Err(refused(Refusal::NotMember));
            }
            let (queue, picked, fresh) = pick_entry(
                &mut tx, account, &group, &entries, pick,
            )
            .await?;
            if let Some(fresh) = fresh {
                entries = fresh;
            }
            let list = blocked_playlist(
                &mut tx, account, &entries,
            )
            .await?;
            group
                .jump(
                    device, queue, &list, picked, at,
                    at as u64,
                )
                .map_err(refused)?;
        }
        Intent::Transport(op) => {
            let list = blocked_playlist(
                &mut tx, account, &entries,
            )
            .await?;
            transport(&mut group, device, &list, op, at)
                .map_err(refused)?;
        }
        Intent::Outputs { outputs, seed } => {
            // 新点的设备不在线就拒;原来就在出声、此刻不在线的直接剔掉(#165):加入是在
            // 组现有的出声设备上追加,离线的旧设备(换了 id 的开发实例、没人用的旧设备)
            // 留在里面会让每一次加入都被拒。
            let outputs: Vec<String> = {
                let online =
                    roster.lock().expect("名册锁中毒");
                let live = |id: &String| {
                    online.device(account, id).is_some()
                };
                if outputs.iter().any(|id| {
                    !live(id) && !group.outputs.contains(id)
                }) {
                    return Err(AppError::Invalid(
                        "有设备不在线",
                    ));
                }
                outputs.into_iter().filter(live).collect()
            };
            group.set_outputs(device, outputs);
            if let (None, Some(seed)) = (&group.now, seed) {
                entries = queue::lock_whole(
                    &mut tx,
                    account,
                    seed.queue_id,
                    seed.revision,
                )
                .await?;
                let list = blocked_playlist(
                    &mut tx, account, &entries,
                )
                .await?;
                group
                    .seed(
                        (seed.queue_id, seed.revision),
                        &list,
                        seed.entry_id,
                        (
                            seed.position_ms * 1_000,
                            seed.playing,
                        ),
                        at,
                    )
                    .map_err(refused)?;
            }
        }
        Intent::Leave => group.leave(device),
        Intent::Append { queue, tracks } => {
            // 已在队列里的(按平台与 id 认)不再进,与本机队列的续取同一个规矩
            let mut inputs: Vec<EntryInput> = entries
                .iter()
                .map(|entry| input(&track_of(entry)))
                .collect();
            for track in &tracks {
                let fresh = input(track);
                if !inputs.iter().any(|held| {
                    (&held.platform, &held.track_id)
                        == (
                            &fresh.platform,
                            &fresh.track_id,
                        )
                }) {
                    inputs.push(fresh);
                }
            }
            if inputs.len() == entries.len() {
                return Err(AppError::Invalid(
                    "续来的歌都已经在组队列里",
                ));
            }
            let published = queue::publish(
                &mut tx, account, queue.0, queue.1, &inputs,
            )
            .await?;
            group
                .extend(
                    device,
                    queue,
                    (
                        published.queue_id,
                        published.revision,
                    ),
                    &published.entry_ids[entries.len()..],
                )
                .map_err(refused)?;
            entries = queue::lock_whole(
                &mut tx,
                account,
                published.queue_id,
                published.revision,
            )
            .await?;
        }
    }

    {
        let list =
            blocked_playlist(&mut tx, account, &entries)
                .await?;
        let online = roster.lock().expect("名册锁中毒");
        group.pause_if_silent(
            &list,
            |id| online.device(account, id).is_some(),
            at,
        );
    }
    let state =
        commit(tx, account, before, group, &entries)
            .await?;
    broadcast(roster, account, &state);
    Ok(state)
}

pub async fn advance(
    pool: &PgPool,
    roster: &SharedRoster,
    account: AccountId,
    device: &str,
    entry_id: i64,
    version: i64,
) -> Result<Option<GroupStateDto>, AppError> {
    let mut tx = pool.begin().await?;
    let mut group = rows::lock(&mut tx, account).await?;
    let at = wall_now_us();
    let entries =
        current_entries(&mut tx, account, &mut group)
            .await?;
    let list = blocked_playlist(&mut tx, account, &entries)
        .await?;
    let before = started(&group);
    let rolled = group.roll(&list, at);
    let advanced = group
        .advance(device, &list, entry_id, version, at)
        .map_err(refused)?;
    if !rolled && !advanced {
        tx.commit().await?;
        return Ok(dto(&group, &entries, &list));
    }
    tracing::info!(
        account,
        device = %device,
        entry_id,
        "出声设备放完,组推进到下一首"
    );
    let state =
        commit(tx, account, before, group, &entries)
            .await?;
    broadcast(roster, account, &state);
    Ok(state)
}

/// 各组这一版里报过就绪的出声设备(#154)。
///
/// 只在内存里:起播最多等 [`timeline::START_WAIT_US`],服务端这时重启了也只是等满上限。
/// 版本一变整份作废 —— 每条意图都加版本,别的意图插进来之后各台照新的一版再报一次。
static READY: LazyLock<Mutex<ReadyTable>> =
    LazyLock::new(Mutex::default);

/// 账号 → (哪一版, 这一版报过就绪的出声设备)。
type ReadyTable = HashMap<AccountId, (i64, Vec<String>)>;

/// 记下 `device` 在 `version` 这一版报了就绪,返回这一版报过的全部设备。
fn mark_ready(
    account: AccountId,
    version: i64,
    device: &str,
) -> Vec<String> {
    let mut all = READY.lock().expect("就绪表锁中毒");
    let held = all.entry(account).or_default();
    if held.0 != version {
        *held = (version, Vec::new());
    }
    if !held.1.iter().any(|id| id == device) {
        held.1.push(device.to_owned());
    }
    held.1.clone()
}

pub async fn ready(
    pool: &PgPool,
    roster: &SharedRoster,
    account: AccountId,
    device: &str,
    entry_id: i64,
    version: i64,
) -> Result<Option<GroupStateDto>, AppError> {
    let mut tx = pool.begin().await?;
    let mut group = rows::lock(&mut tx, account).await?;
    let at = wall_now_us();
    let entries =
        current_entries(&mut tx, account, &mut group)
            .await?;
    let list = blocked_playlist(&mut tx, account, &entries)
        .await?;
    let mut before = started(&group);
    let rolled = group.roll(&list, at);
    let ready = mark_ready(account, version, device);
    let moved = {
        let online = roster.lock().expect("名册锁中毒");
        group.ready(
            device,
            &ready,
            entry_id,
            version,
            |id| online.device(account, id).is_some(),
            at,
        )
    }
    .map_err(refused)?;
    if !rolled && !moved {
        tx.commit().await?;
        return Ok(dto(&group, &entries, &list));
    }
    // 只是同一首提前开走,不是新起了一首:不再记一次 `play_events`。
    if !rolled {
        before = started(&group);
    }
    tracing::info!(
        account,
        device = %device,
        entry_id,
        "出声设备都就绪,起播提前"
    );
    let state =
        commit(tx, account, before, group, &entries)
            .await?;
    broadcast(roster, account, &state);
    Ok(state)
}

/// 设备出册:它若是出声设备、而组里再没有一台出声设备在线,立刻暂停,位置记在这一刻
/// (掉线规则,用户 2026-09-26)。只当遥控器的设备走了什么都不动。
pub async fn device_left(
    pool: &PgPool,
    roster: &SharedRoster,
    account: AccountId,
    device: &str,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let mut group = rows::lock(&mut tx, account).await?;
    let entries =
        current_entries(&mut tx, account, &mut group)
            .await?;
    if !group.outputs.iter().any(|id| id == device) {
        tx.commit().await?;
        return Ok(());
    }
    let at = wall_now_us();
    let list = playlist(&entries);
    let before = started(&group);
    let rolled = group.roll(&list, at);
    let paused = {
        let online = roster.lock().expect("名册锁中毒");
        group.pause_if_silent(
            &list,
            |id| online.device(account, id).is_some(),
            at,
        )
    };
    if !rolled && !paused {
        tx.commit().await?;
        return Ok(());
    }
    tracing::info!(
        account,
        device = %device,
        paused,
        "出声设备出册,组状态随之更新"
    );
    let state =
        commit(tx, account, before, group, &entries)
            .await?;
    broadcast(roster, account, &state);
    Ok(())
}

/// 设备入册后推给它一份当前状态 —— 重连不需要任何「续权」,拿到最新状态照着做。
pub async fn greet(
    pool: &PgPool,
    roster: &SharedRoster,
    account: AccountId,
    device: &str,
) -> Result<(), AppError> {
    let state = current(pool, account).await?;
    let online = roster.lock().expect("名册锁中毒");
    if let Some(sink) = online.sink(account, device) {
        let _ = sink.try_send(ServerSignal::GroupState {
            state: state.map(Box::new),
        });
    }
    Ok(())
}

/// 组此刻的样子。已失效的播放引用在同一事务内清空,GET 与入册一致。
pub async fn current(
    pool: &PgPool,
    account: AccountId,
) -> Result<Option<GroupStateDto>, AppError> {
    let mut tx = pool.begin().await?;
    if rows::load(&mut tx, account).await?.is_none() {
        return Ok(None);
    }
    let mut group = rows::lock(&mut tx, account).await?;
    let entries =
        current_entries(&mut tx, account, &mut group)
            .await?;
    let list = blocked_playlist(&mut tx, account, &entries)
        .await?;
    tx.commit().await?;
    group.roll(&list, wall_now_us());
    Ok(dto(&group, &entries, &list))
}

/// 起一个后台任务:先把服务端挂掉时还在放的组补暂停,之后兜底续播、记心跳。与进程同寿。
pub fn spawn_roller(pool: PgPool, roster: SharedRoster) {
    tokio::spawn(async move {
        if let Err(error) = pause_stranded(&pool).await {
            tracing::warn!(
                ?error,
                "启动时没能暂停上次还在放的组"
            );
        }
        let mut tick = tokio::time::interval(ROLL_EVERY);
        let mut beats = 0u32;
        loop {
            tick.tick().await;
            beats = (beats + 1) % BEAT_EVERY;
            if beats == 0
                && let Err(error) = beat(&pool).await
            {
                tracing::warn!(?error, "组的心跳没记上");
            }
            if let Err(error) =
                roll_due(&pool, &roster).await
            {
                tracing::warn!(
                    ?error,
                    "组的自动续播这一拍失败"
                );
            }
        }
    });
}

async fn beat(pool: &PgPool) -> Result<(), AppError> {
    let mut conn = pool.acquire().await?;
    rows::beat(&mut conn, wall_now_us()).await
}

/// 服务端挂掉时,所有出声设备都在那一刻断开了:照掉线规则 ②,把还在放的组暂停在
/// 最近一次心跳那一刻(没有心跳的就是此刻)。启动时一台设备都还没连上,不必广播。
pub async fn pause_stranded(
    pool: &PgPool,
) -> Result<(), AppError> {
    let stranded = {
        let mut conn = pool.acquire().await?;
        rows::stranded(&mut conn).await?
    };
    let now = wall_now_us();
    for (account, alive) in stranded {
        let at = alive.map_or(now, |alive| alive.min(now));
        pause_stranded_one(pool, account, at).await?;
    }
    Ok(())
}

/// [`pause_stranded`] 的一个组:在放就暂停,位置记在挂钟 `at`。返回有没有改。
pub async fn pause_stranded_one(
    pool: &PgPool,
    account: AccountId,
    at: i64,
) -> Result<bool, AppError> {
    let mut tx = pool.begin().await?;
    let mut group = rows::lock(&mut tx, account).await?;
    let entries =
        current_entries(&mut tx, account, &mut group)
            .await?;
    let before = started(&group);
    if !group.pause_if_silent(
        &playlist(&entries),
        |_| false,
        at,
    ) {
        tx.commit().await?;
        return Ok(false);
    }
    tracing::info!(
        account,
        at,
        "上次服务端退出时组还在放:暂停在最后一次心跳那一刻"
    );
    commit(tx, account, before, group, &entries).await?;
    Ok(true)
}

async fn roll_due(
    pool: &PgPool,
    roster: &SharedRoster,
) -> Result<(), AppError> {
    let due = {
        let mut conn = pool.acquire().await?;
        rows::due(&mut conn, wall_now_us()).await?
    };
    for account in due {
        let mut tx = pool.begin().await?;
        let mut group =
            rows::lock(&mut tx, account).await?;
        let entries =
            current_entries(&mut tx, account, &mut group)
                .await?;
        let list =
            blocked_playlist(&mut tx, account, &entries)
                .await?;
        let before = started(&group);
        if !group.roll(&list, wall_now_us()) {
            tx.commit().await?;
            continue;
        }
        let state =
            commit(tx, account, before, group, &entries)
                .await?;
        broadcast(roster, account, &state);
    }
    Ok(())
}

/// 版本加一、写回、提交,换成线上的样子。
///
/// 这一版若是新起播的一首(换了条目或重新从头放),顺手记一条起播(`play_events`):
/// 组里几台一起响还是那一次,由服务端记,就不必再挑一台设备来报(#137 ⑤ 的 AC-5.4)。
async fn commit(
    mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
    account: AccountId,
    before: Option<(i64, i64)>,
    mut group: Group,
    entries: &[Entry],
) -> Result<Option<GroupStateDto>, AppError> {
    group.version += 1;
    if let Some(now) = &group.now
        && now.playing
        && now.position_us == 0
        && before
            != Some((now.entry_id, now.anchor_wall_us))
        && let Some(entry) = entries
            .iter()
            .find(|e| e.entry_id == now.entry_id)
    {
        crate::store::history::record(
            &mut tx,
            account,
            &crate::store::playlist::TrackRef {
                platform: entry.platform.clone(),
                track_id: entry.track_id.clone(),
            },
        )
        .await?;
    }
    let list =
        blocked_playlist(&mut tx, account, entries).await?;
    let boundary = group.now.as_ref().and_then(|now| {
        list.duration_of(now.entry_id)
            .and_then(|duration| now.deadline(duration))
    });
    rows::save(&mut tx, account, &group, boundary).await?;
    tx.commit().await?;
    Ok(dto(&group, entries, &list))
}

/// 推给账号下每台在线设备。成员与否由收的那一侧看:不在组里的设备也要知道组在放什么
/// (输出设备那一排芯片、「与 X、Y 一起播放」)。
fn broadcast(
    roster: &SharedRoster,
    account: AccountId,
    state: &Option<GroupStateDto>,
) {
    let online = roster.lock().expect("名册锁中毒");
    for sink in online.sinks(account) {
        let _ = sink.try_send(ServerSignal::GroupState {
            state: state.clone().map(Box::new),
        });
    }
}

/// 固定组引用直到事务提交;确认版/条目失效时持久清空,保留成员输出。
async fn current_entries(
    tx: &mut queue::Tx<'_>,
    account: AccountId,
    group: &mut Group,
) -> Result<Vec<Entry>, AppError> {
    let Some(now) = &group.now else {
        return Ok(Vec::new());
    };
    let (queue_id, revision, entry_id) =
        (now.queue_id, now.revision, now.entry_id);
    let entries = match queue::lock_whole(
        tx, account, queue_id, revision,
    )
    .await
    {
        Ok(entries) => entries,
        Err(AppError::NotFound) => {
            let owner: Option<i64> = sqlx::query_scalar("SELECT account_id FROM play_queues WHERE id = $1")
                .bind(queue_id).fetch_optional(&mut **tx).await?;
            if owner.is_some_and(|owner| owner != account) {
                return Err(AppError::NotFound);
            }
            Vec::new()
        }
        Err(err) => return Err(err),
    };
    if entries
        .iter()
        .any(|entry| entry.entry_id == entry_id)
    {
        return Ok(entries);
    }
    group.now = None;
    group.version += 1;
    rows::save(tx, account, group, None).await?;
    tracing::info!(
        account,
        queue_id,
        revision,
        entry_id,
        "清除组的失效播放引用"
    );
    Ok(Vec::new())
}

/// 点的是哪一条:返回(队列, 版本)、条目号,以及换了一版时的新条目。
async fn pick_entry(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    account: AccountId,
    group: &Group,
    entries: &[Entry],
    pick: GroupPickDto,
) -> Result<((i64, i64), i64, Option<Vec<Entry>>), AppError>
{
    match pick {
        GroupPickDto::Entry {
            queue_id,
            revision,
            entry_id,
        } => {
            let same =
                group.now.as_ref().is_some_and(|now| {
                    (now.queue_id, now.revision)
                        == (queue_id, revision)
                });
            let fresh = if same {
                None
            } else {
                Some(
                    queue::lock_whole(
                        tx, account, queue_id, revision,
                    )
                    .await?,
                )
            };
            Ok(((queue_id, revision), entry_id, fresh))
        }
        GroupPickDto::Tracks { tracks, index } => {
            if index >= tracks.len() {
                return Err(AppError::Invalid(
                    "点的那首不在这一批里",
                ));
            }
            // 同一批不重复发布(#137 ③):组手上这一版就是用户眼前这一批,只换条目。
            if let Some(now) = &group.now
                && same_batch(entries, &tracks)
            {
                return Ok((
                    (now.queue_id, now.revision),
                    entries[index].entry_id,
                    None,
                ));
            }
            let inputs: Vec<EntryInput> =
                tracks.iter().map(input).collect();
            let published = queue::create(
                tx,
                account,
                GROUP_QUEUE_DEVICE,
                &inputs,
            )
            .await?;
            let entry_id = published.entry_ids[index];
            let fresh = queue::lock_whole(
                tx,
                account,
                published.queue_id,
                published.revision,
            )
            .await?;
            Ok((
                (published.queue_id, published.revision),
                entry_id,
                Some(fresh),
            ))
        }
    }
}

fn transport(
    group: &mut Group,
    device: &str,
    list: &Playlist,
    op: TransportOpDto,
    at: i64,
) -> Result<(), Refusal> {
    match op {
        TransportOpDto::Pause => {
            group.pause(device, list, at)
        }
        TransportOpDto::Resume => {
            group.resume(device, list, at)
        }
        TransportOpDto::Next => {
            group.step(device, list, 1, at)
        }
        TransportOpDto::Prev => {
            group.step(device, list, -1, at)
        }
        TransportOpDto::Seek { position_ms } => {
            group.seek(device, position_ms * 1_000, at)
        }
        TransportOpDto::Shuffle { on } => {
            group.shuffle(device, list, on, at as u64)
        }
        TransportOpDto::Loop { mode } => {
            group.set_loop(device, mode)
        }
    }
}

fn refused(refusal: Refusal) -> AppError {
    AppError::Invalid(match refusal {
        Refusal::NotMember => "本机不在组里",
        Refusal::Idle => "组里还没有歌",
        Refusal::NoSuchEntry => "那一首不在组队列里",
        Refusal::Stale => "组里已经换了歌",
    })
}

fn same_batch(
    entries: &[Entry],
    tracks: &[TrackDto],
) -> bool {
    entries.len() == tracks.len()
        && entries.iter().zip(tracks).all(
            |(entry, track)| {
                entry.platform == track.platform
                    && entry.track_id == track.id
            },
        )
}

fn input(track: &TrackDto) -> EntryInput {
    EntryInput {
        artist_identities: track.artist_identities.clone(),
        platform: track.platform.clone(),
        track_id: track.id.clone(),
        title: track.title.clone(),
        alias: track.alias.clone(),
        artists: track.artists.clone(),
        cover: track.cover.clone(),
        duration_ms: track.duration_ms,
    }
}

fn track_of(entry: &Entry) -> TrackDto {
    TrackDto {
        artist_identities: entry.artist_identities.clone(),
        platform: entry.platform.clone(),
        id: entry.track_id.clone(),
        title: entry.title.clone(),
        alias: entry.alias.clone(),
        artists: entry.artists.clone(),
        cover: entry.cover.clone(),
        duration_ms: entry.duration_ms,
        album: None,
        facets: Default::default(),
    }
}

/// 这一刻放的是哪一条、锚在哪 —— 与改完之后比,认出「新起播了一首」。
fn started(group: &Group) -> Option<(i64, i64)> {
    group
        .now
        .as_ref()
        .map(|now| (now.entry_id, now.anchor_wall_us))
}

fn playlist(entries: &[Entry]) -> Playlist {
    Playlist {
        entries: entries
            .iter()
            .map(|entry| {
                (
                    entry.entry_id,
                    entry.duration_ms.max(0) as u64 * 1_000,
                    false,
                )
            })
            .collect(),
    }
}

/// 组接下来该放哪一条要认账号的屏蔽规则(#167):没规则时与 [`playlist`] 等价,一条
/// 查询按 [`crate::store::blocks::hits`] 同一套口径给每条标上「命不命中」,标签命中
/// 走与列表出口([`crate::store::facets::fill`])同一份聚合。
async fn blocked_playlist(
    conn: &mut PgConnection,
    account: AccountId,
    entries: &[Entry],
) -> Result<Playlist, AppError> {
    let rules = blocks::list(conn, account).await?;
    if rules.is_empty() {
        return Ok(playlist(entries));
    }
    let mut tracks: Vec<TrackDto> =
        entries.iter().map(track_of).collect();
    facets::fill(conn, account, &mut tracks).await?;
    Ok(Playlist {
        entries: entries
            .iter()
            .zip(&tracks)
            .map(|(entry, track)| {
                (
                    entry.entry_id,
                    entry.duration_ms.max(0) as u64 * 1_000,
                    blocks::hits(&rules, track),
                )
            })
            .collect(),
    })
}

/// 换成线上的样子。时刻从挂钟换到服务端单调钟:锚点已经过去的,重新锚在「现在」。
fn dto(
    group: &Group,
    entries: &[Entry],
    list: &Playlist,
) -> Option<GroupStateDto> {
    if group.is_vacant() {
        return None;
    }
    let (wall, mono) =
        (wall_now_us(), clock::now_us() as i64);
    let to_mono =
        |at: i64| (mono + (at - wall)).max(0) as u64;
    let now = group.now.as_ref().and_then(|now| {
        let entry = entries
            .iter()
            .find(|e| e.entry_id == now.entry_id)?;
        let duration = list.duration_of(now.entry_id)?;
        let (anchor_us, position_us) = if now.playing
            && now.anchor_wall_us > wall
        {
            (to_mono(now.anchor_wall_us), now.position_us)
        } else {
            (mono as u64, now.position_at(wall, duration))
        };
        let next = now
            .boundary(duration)
            .zip(now.follower(list))
            .and_then(|(end, id)| {
                let entry = entries
                    .iter()
                    .find(|e| e.entry_id == id)?;
                Some(NextEntryDto {
                    entry_id: id,
                    track: track_of(entry),
                    at_us: to_mono(end),
                })
            });
        Some(GroupNowDto {
            queue_id: now.queue_id,
            revision: now.revision,
            entry_id: now.entry_id,
            track: track_of(entry),
            anchor_us,
            position_us,
            playing: now.playing,
            next,
            at_end: now.at_end(list),
            shuffled: now.shuffled,
            loop_mode: now.loop_mode,
        })
    });
    Some(GroupStateDto {
        version: group.version as u64,
        members: group.members.clone(),
        outputs: group.outputs.clone(),
        clock_epoch: clock::epoch(),
        now,
    })
}
