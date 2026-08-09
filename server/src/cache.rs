//! 平台曲目的缓存。
//!
//! **不是镜像**(见 `docs/adr/0018`)。三条规矩把它钉在缓存这一侧,少一条它就
//! 变回镜像:写永远直发平台、冲突时平台赢、整张删掉只是慢一次而不丢东西。
//! 判定问题 —— 删了会丢数据吗?会,那它已经不是缓存了。
//!
//! 这个模块本身因此只有两个动作:**整体覆盖**一个歌单,和**读回来**。没有
//! 「往缓存里加一首」这种接口 —— 那是写,而写不在这里发生。

use contract::TrackDto;
use sqlx::{PgConnection, QueryBuilder};

use crate::error::AppError;

/// 「我喜欢的」在缓存里的歌单标识。
///
/// 取空串:平台的歌单 id 恒非空,撞不上。红心在这张表里就是个普通歌单,
/// 不单开一套表,也不单写一套代码。
pub const LIKED_PLAYLIST_ID: &str = "";

/// 一次写多少行。
///
/// Postgres 一条语句最多 65535 个参数,而每首歌占 7 个 —— 973 首的歌单
/// 一次写不完。分批的批量插入仍然远快过一首一条语句。
const ROWS_PER_STATEMENT: usize = 1000;

/// 把一个歌单的曲目整体写进缓存,替换原先那份。
///
/// 替换而不是追加:平台那边删掉的歌,这边刷新之后也该没有它。追加的话删掉的
/// 歌会永远留着,而且每刷一次列表就长一截。
///
/// 调用方应当把它和自己的其余写操作放在同一个事务里 —— 成员关系删掉了而新的
/// 没插进去,那一瞬间的歌单是空的。
pub async fn set_playlist(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: &str,
    tracks: &[TrackDto],
) -> Result<(), AppError> {
    put_details(conn, tracks).await?;

    let platform = match tracks.first() {
        Some(track) => track.platform.as_str(),
        // 一首都没有,成员关系照样要清空 —— 平台把歌单清空了也是一次刷新。
        // 平台名此时没有意义:没有行要插进去。
        None => "",
    };
    // 这条路上没有加入时间:调用方只给了曲目详情,而加入时刻是**成员关系**的
    // 属性,详情里没有它。这些行按 NULLS LAST 退回 position 次序。
    let refs: Vec<TrackRef> = tracks
        .iter()
        .map(|track| TrackRef::new(&track.id, None))
        .collect();

    set_membership(
        conn,
        account_id,
        playlist_id,
        platform,
        &refs,
    )
    .await
}

/// 歌单里的一条成员关系:哪首歌,什么时候被加进来的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRef {
    pub id: String,
    /// 加入这个歌单的时刻,平台给的毫秒时间戳。
    ///
    /// `None` 表示平台没给。那样它排在有时间的之后,彼此仍按 `position` 稳定 ——
    /// 不编一个时间出来:错的顺序不会有任何人报错。
    pub added_at_ms: Option<i64>,
}

impl TrackRef {
    pub fn new(id: &str, added_at_ms: Option<i64>) -> Self {
        Self {
            id: id.to_owned(),
            // 平台用 0 表示「没有」,这一层把它归一成 None ——
            // 0 会被 to_timestamp 翻成 1970,那是个会参与排序的真实时刻
            added_at_ms: added_at_ms.filter(|ms| *ms != 0),
        }
    }
}

/// 写一个歌单的成员关系与次序,替换原先那份。
///
/// 与 [`set_playlist`] 的差别是它**不带详情**:读路径上常态是「973 个 id,
/// 其中 972 个详情早就在库里」,把那 972 首重取一遍就等于没有缓存。
///
/// 传进来的 id 必须已经有详情(见 [`missing_details`]),否则外键会拒绝 ——
/// 那是有意的:悄悄插进去的话它会在读回时的 JOIN 里消失,歌单少一首而没有
/// 任何人报错。
pub async fn set_membership(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: &str,
    platform: &str,
    refs: &[TrackRef],
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM platform_playlist_tracks
         WHERE account_id = $1 AND playlist_id = $2",
    )
    .bind(account_id)
    .bind(playlist_id)
    .execute(&mut *conn)
    .await?;

    for (offset, chunk) in
        refs.chunks(ROWS_PER_STATEMENT).enumerate()
    {
        let base = (offset * ROWS_PER_STATEMENT) as i64;

        let mut query = QueryBuilder::new(
            "INSERT INTO platform_playlist_tracks
             (account_id, playlist_id, platform, track_id, position, added_at) ",
        );
        query.push_values(
            chunk.iter().enumerate(),
            |mut row, (i, track)| {
                row.push_bind(account_id)
                    .push_bind(playlist_id)
                    .push_bind(platform)
                    .push_bind(&track.id)
                    .push_bind(base + i as i64);
                // 毫秒转 TIMESTAMPTZ 交给库:这个 crate 的 sqlx 没开时间
                // feature,为一个排序键开它不值得。NULL 一路传到底。
                row.push("to_timestamp(");
                row.push_bind_unseparated(
                    track.added_at_ms,
                )
                .push_unseparated(
                    "::double precision / 1000)",
                );
            },
        );
        // 同一首歌在一个歌单里出现两次是平台的事,不该让整次刷新失败
        query.push(" ON CONFLICT DO NOTHING");

        query.build().execute(&mut *conn).await?;
    }

    Ok(())
}

/// 这些 id 里哪些还没有详情。
///
/// 读路径靠它只向平台要缺的那些。红心里点一个心,变的只有成员关系,
/// 详情一条都不必重取 —— 这条错了,每点一次心就是一次全量拉取。
pub async fn missing_details(
    conn: &mut PgConnection,
    platform: &str,
    ids: &[String],
) -> Result<Vec<String>, AppError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    // 反过来问「哪些有」再在内存里做差集也可以,但那要把已有的几百条 id
    // 传回来一趟。让库做差集,回来的只有真正缺的那些。
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT asked.id
         FROM UNNEST($1::text[]) AS asked (id)
         WHERE NOT EXISTS (
             SELECT 1 FROM platform_tracks d
             WHERE d.platform = $2 AND d.track_id = asked.id
         )",
    )
    .bind(ids)
    .bind(platform)
    .fetch_all(conn)
    .await?;

    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// 按给定的 id 顺序读详情,与歌单无关。
///
/// 本地歌单用这条:它的成员关系真相在自家的 `local_playlist_tracks`,要借的只有
/// 详情那一半。走 [`tracks_of`] 的话就得先把成员关系写进 `platform_playlist_tracks`,
/// 而那张表的 `playlist_id` 是 TEXT —— 本地歌单的整数 id 会和平台歌单的字符串 id
/// 撞在同一列上。
///
/// 没有详情的 id 直接跳过:平台不肯给的歌(下架、无权限)就是这样,而让整个歌单
/// 打不开比少一首更坏。
pub async fn details_of(
    conn: &mut PgConnection,
    platform: &str,
    ids: &[String],
) -> Result<Vec<TrackDto>, AppError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    // 顺序由请求方给,不是表里的任何一列 —— 用 UNNEST 的下标排,
    // 免得把几百条记录取回来再在内存里重排一次。
    let rows: Vec<TrackRow> = sqlx::query_as(
        "SELECT d.platform, d.track_id, d.title, d.alias,
                d.artists, d.cover, d.duration_ms
         FROM UNNEST($1::text[]) WITH ORDINALITY AS asked (id, pos)
         JOIN platform_tracks d
           ON d.platform = $2 AND d.track_id = asked.id
         ORDER BY asked.pos",
    )
    .bind(ids)
    .bind(platform)
    .fetch_all(conn)
    .await?;

    Ok(rows.into_iter().map(TrackRow::into_dto).collect())
}

/// 读回一个歌单的曲目,最近加入的在最前。
///
/// 次序由后端定,不是平台数组的原序(见 `docs/adr/0021`)。`position` 退居
/// 第二排序键:它兜住加入时间相同、以及平台压根没给时间的那些行,让顺序在
/// 两次读之间稳定 —— 否则同一个歌单每次进去的排法都可能不一样。
///
/// 没缓存过就是空列表,不是错误 —— 冷启动走的正是这条路。
pub async fn tracks_of(
    conn: &mut PgConnection,
    account_id: i64,
    playlist_id: &str,
) -> Result<Vec<TrackDto>, AppError> {
    let rows: Vec<TrackRow> = sqlx::query_as(
        "SELECT d.platform, d.track_id, d.title, d.alias,
                d.artists, d.cover, d.duration_ms
         FROM platform_playlist_tracks m
         JOIN platform_tracks d
           ON d.platform = m.platform AND d.track_id = m.track_id
         WHERE m.account_id = $1 AND m.playlist_id = $2
         ORDER BY m.added_at DESC NULLS LAST, m.position",
    )
    .bind(account_id)
    .bind(playlist_id)
    .fetch_all(conn)
    .await?;

    Ok(rows.into_iter().map(TrackRow::into_dto).collect())
}

/// 查出来的一行。
///
/// 用具名结构体而不是元组:七个字段的元组在签名里读不出谁是谁,而
/// `title` 与 `alias` 都是 `String`,顺序错了编译器不会说话。
#[derive(sqlx::FromRow)]
struct TrackRow {
    platform: String,
    /// 列名是 `track_id`,而 [`TrackDto`] 那侧叫 `id`
    track_id: String,
    title: String,
    alias: Option<String>,
    artists: Vec<String>,
    cover: Option<String>,
    duration_ms: i64,
}

impl TrackRow {
    fn into_dto(self) -> TrackDto {
        TrackDto {
            platform: self.platform,
            id: self.track_id,
            title: self.title,
            alias: self.alias,
            artists: self.artists,
            cover: self.cover,
            duration_ms: self.duration_ms,
        }
    }
}

/// 写曲目详情,已有的就覆盖。
///
/// 详情按 (平台, id) 存,与歌单无关 —— 同一首歌在红心里、在三个歌单里,
/// 存的都是这一条。覆盖是必须的:歌名会改、封面会换,刷新就该看到新的。
pub async fn put_details(
    conn: &mut PgConnection,
    tracks: &[TrackDto],
) -> Result<(), AppError> {
    for chunk in tracks.chunks(ROWS_PER_STATEMENT) {
        let mut query = QueryBuilder::new(
            "INSERT INTO platform_tracks
             (platform, track_id, title, alias, artists, cover, duration_ms) ",
        );
        query.push_values(chunk, |mut row, track| {
            row.push_bind(&track.platform)
                .push_bind(&track.id)
                .push_bind(&track.title)
                .push_bind(&track.alias)
                .push_bind(&track.artists)
                .push_bind(&track.cover)
                .push_bind(track.duration_ms);
        });
        query.push(
            " ON CONFLICT (platform, track_id) DO UPDATE SET
                title = EXCLUDED.title,
                alias = EXCLUDED.alias,
                artists = EXCLUDED.artists,
                cover = EXCLUDED.cover,
                duration_ms = EXCLUDED.duration_ms,
                fetched_at = now()",
        );

        query.build().execute(&mut *conn).await?;
    }

    Ok(())
}
