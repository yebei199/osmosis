//! 分页拼装那一段。
//!
//! 只测它,不测那几条 `async fn` 的 URL 与方法 —— 那些没有分支,写错了第一次
//! 联调就会撞见。而这里三种坏情形正相反:都不会在正常联调里出现,每一种都会
//! 让执行端拿到一份**错的**队列,而界面上看起来只是「歌单顺序有点怪」。

use super::*;

fn entry(entry_id: i64) -> QueueEntryDto {
    QueueEntryDto {
        entry_id,
        position: entry_id - 1,
        track: TrackDto {
            platform: "netease".to_owned(),
            id: entry_id.to_string(),
            title: format!("歌 {entry_id}"),
            alias: None,
            artists: vec!["LiSA".to_owned()],
            cover: None,
            duration_ms: 234_000,
        },
    }
}

/// 一页的样子:从 `offset` 起切 `size` 条,`total` 照实报。
fn page_of(
    total: i64,
    offset: i64,
    size: i64,
) -> QueuePageDto {
    let entries = (offset + 1..=total)
        .take(size.max(0) as usize)
        .map(entry)
        .collect();
    QueuePageDto {
        queue_id: 7,
        revision: 3,
        total,
        offset,
        entries,
    }
}

/// 一次取完的那种:一页就够。
#[tokio::test]
async fn a_single_page_is_returned_as_is() {
    let entries = collect_pages(|offset| async move {
        Ok(page_of(3, offset, 500))
    })
    .await
    .expect("一页该取得回来");

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].entry_id, 1);
    assert_eq!(entries[2].entry_id, 3);
}

/// 要翻好几页的那种:拼起来顺序不乱、不重不漏。
///
/// 长队列本来就是这条链路的全部起因(977 首),一页装不下是常态而不是边界。
#[tokio::test]
async fn several_pages_are_stitched_in_order() {
    let entries = collect_pages(|offset| async move {
        Ok(page_of(1_200, offset, 500))
    })
    .await
    .expect("多页该拼得起来");

    assert_eq!(entries.len(), 1_200);
    let ids: Vec<i64> =
        entries.iter().map(|row| row.entry_id).collect();
    assert_eq!(ids, (1..=1_200).collect::<Vec<i64>>());
}

/// 中途一页失败,**整次失败**,不返回半份。
///
/// 半份拿去执行,用户听到的是一个他没点过的队列 —— 而取数失败的正确处置是
/// 保留旧的执行副本(`docs/adr/0031` 七)。
#[tokio::test]
async fn a_failed_page_fails_the_whole_fetch() {
    let outcome = collect_pages(|offset| async move {
        if offset == 0 {
            Ok(page_of(1_200, offset, 500))
        } else {
            Err(ApiError::Transport("断了".to_owned()))
        }
    })
    .await;

    assert!(
        outcome.is_err(),
        "取到一半断了该整次失败,得到的是 {outcome:?}"
    );
}

/// 还没取够却给了空页:报错,不要转起来。
///
/// 死循环在界面上是「一直转圈」,与「网络慢」分不开,而它永远不会自己好。
#[tokio::test]
async fn an_empty_page_short_of_the_total_is_an_error() {
    let outcome = collect_pages(|offset| async move {
        if offset == 0 {
            Ok(page_of(1_200, offset, 500))
        } else {
            Ok(QueuePageDto {
                entries: Vec::new(),
                ..page_of(1_200, offset, 0)
            })
        }
    })
    .await;

    assert!(
        matches!(outcome, Err(ApiError::Decode(_))),
        "取不够又不给条目该报错,得到的是 {outcome:?}"
    );
}

/// 服务端报的 `total` 比实际给的少时,以**给到的**为准,不多要一页。
///
/// 多要那一页会拿到空页,然后被上一条判成错误 —— 而这一种其实没出错:
/// 条目都在手上了。
#[tokio::test]
async fn a_total_smaller_than_the_page_stops_cleanly() {
    let entries = collect_pages(|offset| async move {
        Ok(QueuePageDto {
            total: 2,
            ..page_of(3, offset, 500)
        })
    })
    .await
    .expect("给够了就该收工");

    assert_eq!(entries.len(), 3);
}
