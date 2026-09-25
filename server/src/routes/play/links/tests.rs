//! 直链缓存的命中规则:够放完整首才给、不跨账号、没有期限不记、满了扔最早不能用的。
//!
//! 时间全由调用方传 `Instant`,不睡、不看机器快慢。

use std::time::{Duration, Instant};

use contract::PlaySourceDto;

use server::bangdream::proto::PlaySource;

use super::{CAPACITY, LinkKey, MARGIN, SignedLinks};

/// 网易云实测的形状:有效期 1200 秒。
const EXPIRES_IN: Duration = Duration::from_secs(1200);

fn key(account: i64, track: &str) -> LinkKey {
    (account, track.to_owned(), 2)
}

fn upstream(
    expires_in: Duration,
    duration: Duration,
) -> PlaySource {
    PlaySource {
        url: "https://m8.music.126.net/x.mp3".to_owned(),
        expires_in_seconds: expires_in.as_secs() as i32,
        duration_ms: duration.as_millis() as i64,
        ..PlaySource::default()
    }
}

fn dto(url: &str) -> PlaySourceDto {
    PlaySourceDto {
        url: url.to_owned(),
        format: "mp3".to_owned(),
        bit_rate: 320_000,
        trial: false,
    }
}

/// 五分钟的歌、二十分钟的链:刚拿到时当然给。
#[test]
fn a_fresh_link_long_enough_for_the_whole_track_is_served()
{
    let links = SignedLinks::default();
    let t0 = Instant::now();
    links.record(
        key(1, "a"),
        &upstream(EXPIRES_IN, Duration::from_secs(300)),
        dto("u1"),
        t0,
    );

    assert_eq!(
        links
            .get(&key(1, "a"), t0 + Duration::from_secs(1)),
        Some(dto("u1")),
    );
}

/// 剩下的时间刚好还够「整首 + 余量」时给,再晚一秒就不给 —— 放到一半断链
/// 比多问一次上游糟得多。
#[test]
fn a_link_is_served_only_while_the_rest_covers_the_whole_track()
 {
    let links = SignedLinks::default();
    let t0 = Instant::now();
    let duration = Duration::from_secs(300);
    links.record(
        key(1, "a"),
        &upstream(EXPIRES_IN, duration),
        dto("u1"),
        t0,
    );
    let last = t0 + EXPIRES_IN - duration - MARGIN;

    assert!(
        links
            .get(
                &key(1, "a"),
                last - Duration::from_secs(1)
            )
            .is_some(),
        "剩余有效期还够整首加余量,应当命中",
    );
    assert_eq!(
        links.get(
            &key(1, "a"),
            last + Duration::from_secs(1)
        ),
        None,
        "剩余有效期不够整首加余量,不能命中",
    );
}

/// 有效期一开始就不够放完整首(长歌、短链),一次也不给。
#[test]
fn a_link_shorter_than_the_track_is_never_served() {
    let links = SignedLinks::default();
    let t0 = Instant::now();
    links.record(
        key(1, "a"),
        &upstream(EXPIRES_IN, EXPIRES_IN),
        dto("u1"),
        t0,
    );

    assert_eq!(links.get(&key(1, "a"), t0), None);
}

/// 直链带着账号的签名与权限:A 拿到的不能给 B。
#[test]
fn a_link_is_never_served_to_another_account() {
    let links = SignedLinks::default();
    let t0 = Instant::now();
    links.record(
        key(1, "a"),
        &upstream(EXPIRES_IN, Duration::from_secs(300)),
        dto("u1"),
        t0,
    );

    assert_eq!(links.get(&key(2, "a"), t0), None);
}

/// 上游没说有效期或时长(0),就算不出能用到什么时候,不记。
#[test]
fn a_link_without_expiry_or_duration_is_not_kept() {
    let links = SignedLinks::default();
    let t0 = Instant::now();
    links.record(
        key(1, "a"),
        &upstream(Duration::ZERO, Duration::from_secs(300)),
        dto("u1"),
        t0,
    );
    links.record(
        key(1, "b"),
        &upstream(EXPIRES_IN, Duration::ZERO),
        dto("u2"),
        t0,
    );

    assert_eq!(links.get(&key(1, "a"), t0), None);
    assert_eq!(links.get(&key(1, "b"), t0), None);
}

/// 同一首重新拿到的链替掉旧的。
#[test]
fn a_newer_link_replaces_the_older_one() {
    let links = SignedLinks::default();
    let t0 = Instant::now();
    let later = t0 + Duration::from_secs(60);
    let track =
        upstream(EXPIRES_IN, Duration::from_secs(300));
    links.record(key(1, "a"), &track, dto("u1"), t0);
    links.record(key(1, "a"), &track, dto("u2"), later);

    assert_eq!(
        links.get(&key(1, "a"), later),
        Some(dto("u2"))
    );
}

/// 满了不再无限长:多出来的那条挤掉最早不能用的,刚记的留着。
#[test]
fn a_full_cache_drops_the_link_that_goes_stale_first() {
    let links = SignedLinks::default();
    let t0 = Instant::now();
    let track =
        upstream(EXPIRES_IN, Duration::from_secs(300));
    for i in 0..CAPACITY {
        links.record(
            key(1, &i.to_string()),
            &track,
            dto("old"),
            t0 + Duration::from_millis(i as u64),
        );
    }
    let now = t0 + Duration::from_secs(1);
    links.record(key(1, "new"), &track, dto("new"), now);

    assert_eq!(links.lock().len(), CAPACITY);
    assert_eq!(links.get(&key(1, "0"), now), None);
    assert_eq!(
        links.get(&key(1, "new"), now),
        Some(dto("new"))
    );
    assert_eq!(
        links.get(&key(1, "1"), now),
        Some(dto("old"))
    );
}
