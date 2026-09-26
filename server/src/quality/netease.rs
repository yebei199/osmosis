//! 网易云的档位映射,经 bang-dream 的 `QualityLevel` 过桥。
//!
//! bang-dream 把它再翻成网易云的 `level` 参数(`standard`/`higher`/`exhigh`/
//! `lossless`/`hires`),并把网易云实际给的档位报回来。协商在网易云那边发生:
//! 请求 `hires`,给得出就给 hires,给不出给它能给的最好那一档。

use crate::bangdream::proto::{PlaySource, QualityLevel};

use super::{Quality, Tier, guess_tier};

/// 通用档位 → 请求里的档位。
pub fn level_of(tier: Tier) -> QualityLevel {
    match tier {
        Tier::Low => QualityLevel::Low,
        Tier::Standard => QualityLevel::Standard,
        Tier::High => QualityLevel::High,
        Tier::Lossless => QualityLevel::Lossless,
        Tier::HiRes => QualityLevel::HiRes,
    }
}

/// 报回来的档位 → 通用档位。没报(`UNSPECIFIED`)或认不得的是 `None`。
pub fn tier_of(level: i32) -> Option<Tier> {
    match QualityLevel::try_from(level).ok()? {
        QualityLevel::Unspecified => None,
        QualityLevel::Low => Some(Tier::Low),
        QualityLevel::Standard => Some(Tier::Standard),
        QualityLevel::High => Some(Tier::High),
        QualityLevel::Lossless => Some(Tier::Lossless),
        QualityLevel::HiRes => Some(Tier::HiRes),
    }
}

/// 一次取到的源实际是什么音质。上游没报档位时按格式与码率认。
pub fn quality_of(source: &PlaySource) -> Quality {
    let format = source.format.to_ascii_lowercase();
    Quality {
        tier: tier_of(source.level).unwrap_or_else(|| {
            guess_tier(&format, source.bit_rate)
        }),
        format,
        bit_rate: source.bit_rate,
        bits_per_sample: None,
        sample_rate: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tier_round_trips_through_netease() {
        for tier in Tier::ALL {
            assert_eq!(
                tier_of(level_of(tier) as i32),
                Some(tier)
            );
        }
    }

    #[test]
    fn highest_asks_netease_for_hi_res() {
        assert_eq!(
            level_of(Tier::HIGHEST),
            QualityLevel::HiRes
        );
    }

    #[test]
    fn an_unreported_level_is_not_a_tier() {
        assert_eq!(
            tier_of(QualityLevel::Unspecified as i32),
            None
        );
        assert_eq!(tier_of(99), None);
    }

    /// 请求最高,网易云只给得出 320k:如实报 320k,不按请求冒充。
    #[test]
    fn the_reported_level_wins_over_the_request() {
        let source = PlaySource {
            format: "MP3".to_owned(),
            bit_rate: 320_000,
            level: QualityLevel::High as i32,
            ..PlaySource::default()
        };
        assert_eq!(
            quality_of(&source),
            Quality {
                tier: Tier::High,
                format: "mp3".to_owned(),
                bit_rate: 320_000,
                bits_per_sample: None,
                sample_rate: None,
            }
        );
    }

    #[test]
    fn an_unreported_level_is_read_off_the_format() {
        let source = PlaySource {
            format: "flac".to_owned(),
            bit_rate: 900_000,
            ..PlaySource::default()
        };
        assert_eq!(
            quality_of(&source).tier,
            Tier::Lossless
        );
    }
}
