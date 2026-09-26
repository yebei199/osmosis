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

use std::time::Duration;

use contract::{
    GroupNowDto, GroupPickDto, GroupStateDto, NextEntryDto,
    ServerSignal, TrackDto, TransportOpDto,
};
use sqlx::{PgConnection, PgPool};

use crate::error::AppError;
use crate::store::group as rows;
use crate::store::queue::{self, Entry, EntryInput};
use crate::syncplay::clock;
use crate::syncplay::signaling::{AccountId, SharedRoster};
use timeline::{Group, Playlist, Refusal};

/// 组队列归这台「设备」。组里的点歌都发到它名下那一个队列的新版本上,与各台设备自己
/// 独奏时的队列分开 —— 否则某台退出组后独奏发布的新版本,会把组还在放的那一版挤掉。
pub const GROUP_QUEUE_DEVICE: &str = "group";

/// 放完自动续播多久看一次。每一首的换歌时刻各台设备自己按状态里预告的下一首掐,
/// 这一拍只负责把服务端的状态推到下一首、再广播,晚几百毫秒不影响出声。
const ROLL_EVERY: Duration = Duration::from_millis(500);

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
}

/// 挂钟微秒。时间线落库用它:服务端单调钟每次启动从零起,重启后换算不回来。
pub fn wall_now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_micros() as i64)
}

/// 应用一条意图,返回应用之后组的样子(组散了是 `None`),并推给账号下每台在线设备。
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
        current_entries(&mut tx, account, &group).await?;
    let before = started(&group);
    group.roll(&playlist(&entries), at);

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
            group
                .jump(
                    device,
                    queue,
                    &playlist(&entries),
                    picked,
                    at,
                    at as u64,
                )
                .map_err(refused)?;
        }
        Intent::Transport(op) => {
            transport(&mut group, device, &entries, op, at)
                .map_err(refused)?;
        }
        Intent::Outputs { outputs, seed } => {
            {
                let online =
                    roster.lock().expect("名册锁中毒");
                if outputs.iter().any(|id| {
                    online.device(account, id).is_none()
                }) {
                    return Err(AppError::Invalid(
                        "有设备不在线",
                    ));
                }
            }
            group.set_outputs(device, outputs);
            if let (None, Some(seed)) = (&group.now, seed) {
                entries = queue::whole(
                    &mut tx,
                    account,
                    seed.queue_id,
                    seed.revision,
                )
                .await?;
                group
                    .seed(
                        (seed.queue_id, seed.revision),
                        &playlist(&entries),
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
    }

    {
        let online = roster.lock().expect("名册锁中毒");
        group.pause_if_silent(
            &playlist(&entries),
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
    if !group.outputs.iter().any(|id| id == device) {
        return Ok(());
    }
    let at = wall_now_us();
    let entries =
        current_entries(&mut tx, account, &group).await?;
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

/// 组此刻的样子(不改库)。`GET /group` 与入册推送用它。
pub async fn current(
    pool: &PgPool,
    account: AccountId,
) -> Result<Option<GroupStateDto>, AppError> {
    let mut conn = pool.acquire().await?;
    let Some(mut group) =
        rows::load(&mut conn, account).await?
    else {
        return Ok(None);
    };
    let entries =
        current_entries(&mut conn, account, &group).await?;
    group.roll(&playlist(&entries), wall_now_us());
    Ok(dto(&group, &entries))
}

/// 起一个后台任务:放完的组往下推一首,再广播。与进程同寿。
pub fn spawn_roller(pool: PgPool, roster: SharedRoster) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(ROLL_EVERY);
        loop {
            tick.tick().await;
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
            current_entries(&mut tx, account, &group)
                .await?;
        let before = started(&group);
        if !group.roll(&playlist(&entries), wall_now_us()) {
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
    let boundary = group.now.as_ref().and_then(|now| {
        playlist(entries)
            .duration_of(now.entry_id)
            .and_then(|duration| now.boundary(duration))
    });
    rows::save(&mut tx, account, &group, boundary).await?;
    tx.commit().await?;
    Ok(dto(&group, entries))
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

/// 组此刻那一版队列的全部条目。组里还没有歌时是空的。
async fn current_entries(
    conn: &mut PgConnection,
    account: AccountId,
    group: &Group,
) -> Result<Vec<Entry>, AppError> {
    match &group.now {
        Some(now) => {
            queue::whole(
                conn,
                account,
                now.queue_id,
                now.revision,
            )
            .await
        }
        None => Ok(Vec::new()),
    }
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
                    queue::whole(
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
            let fresh = queue::whole(
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
    entries: &[Entry],
    op: TransportOpDto,
    at: i64,
) -> Result<(), Refusal> {
    let list = playlist(entries);
    match op {
        TransportOpDto::Pause => {
            group.pause(device, &list, at)
        }
        TransportOpDto::Resume => {
            group.resume(device, &list, at)
        }
        TransportOpDto::Next => {
            group.step(device, &list, 1, at)
        }
        TransportOpDto::Prev => {
            group.step(device, &list, -1, at)
        }
        TransportOpDto::Seek { position_ms } => {
            group.seek(device, position_ms * 1_000, at)
        }
        TransportOpDto::Shuffle { on } => {
            group.shuffle(device, &list, on, at as u64)
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
        platform: entry.platform.clone(),
        id: entry.track_id.clone(),
        title: entry.title.clone(),
        alias: entry.alias.clone(),
        artists: entry.artists.clone(),
        cover: entry.cover.clone(),
        duration_ms: entry.duration_ms,
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
                )
            })
            .collect(),
    }
}

/// 换成线上的样子。时刻从挂钟换到服务端单调钟:锚点已经过去的,重新锚在「现在」。
fn dto(
    group: &Group,
    entries: &[Entry],
) -> Option<GroupStateDto> {
    if group.is_vacant() {
        return None;
    }
    let (wall, mono) =
        (wall_now_us(), clock::now_us() as i64);
    let to_mono =
        |at: i64| (mono + (at - wall)).max(0) as u64;
    let list = playlist(entries);
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
            .zip(now.follower(&list))
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
