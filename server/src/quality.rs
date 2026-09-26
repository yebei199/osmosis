//! 音质:与音源无关的「档位」与「实际音质」(#147,`docs/adr/0034`)。
//!
//! 档位是**想要什么**,实际音质是**拿到的是什么**。两者分开:请求无损,音源
//! 可能只给得出 320k,那时如实报 320k,不按请求的档位冒充。
//!
//! 每个音源一份双向映射(见 [`netease`]):通用档位翻成它自己的请求参数,它报回
//! 来的档位翻回通用档位。新音源接进来只写这一份映射,本模块与调用方都不动。

pub mod netease;

use contract::QualityDto;

/// 档位。有序:越往后越好,`>=` 就是「至少这么好」。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
)]
pub enum Tier {
    Low,
    Standard,
    High,
    Lossless,
    HiRes,
}

impl Tier {
    /// 「最高」:要音源给出它能给的最好那一档。
    pub const HIGHEST: Self = Self::HiRes;

    /// 全部档位,从低到高。
    pub const ALL: [Self; 5] = [
        Self::Low,
        Self::Standard,
        Self::High,
        Self::Lossless,
        Self::HiRes,
    ];

    /// 线上格式与库里的写法,如 `lossless`。
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Standard => "standard",
            Self::High => "high",
            Self::Lossless => "lossless",
            Self::HiRes => "hi_res",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|tier| tier.name() == name)
    }

    pub fn is_lossless(self) -> bool {
        self >= Self::Lossless
    }
}

/// 实际拿到的音质。
///
/// 位深与采样率只有看得到文件头时才知道(见 [`flac_stream_info`]),
/// 音源不报就是 `None`,不编一个默认值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quality {
    pub tier: Tier,
    /// 容器格式,小写,如 `flac`。
    pub format: String,
    /// 码率,bit/s。
    pub bit_rate: i32,
    pub bits_per_sample: Option<i32>,
    pub sample_rate: Option<i32>,
}

impl Quality {
    pub fn to_dto(&self) -> QualityDto {
        QualityDto {
            tier: self.tier.name().to_owned(),
            format: self.format.clone(),
            bit_rate: self.bit_rate,
            bits_per_sample: self.bits_per_sample,
            sample_rate: self.sample_rate,
        }
    }
}

/// 音源没报档位时,按格式与码率认一个。
///
/// 无损容器至少是无损;有损的按码率落档,门槛取常见的 128k / 192k / 320k。
pub fn guess_tier(format: &str, bit_rate: i32) -> Tier {
    match format.to_ascii_lowercase().as_str() {
        "flac" | "wav" | "ape" | "alac" => Tier::Lossless,
        _ if bit_rate >= 320_000 => Tier::High,
        _ if bit_rate >= 192_000 => Tier::Standard,
        _ => Tier::Low,
    }
}

/// FLAC 文件头里的位深与采样率。不是 FLAC、或头不完整,就是 `None`。
///
/// 布局:`fLaC` 四字节,紧跟第一个元数据块(必是 STREAMINFO):四字节块头,
/// 块体第 10 字节起 20 位采样率、3 位声道数减一、5 位位深减一。
pub fn flac_stream_info(
    bytes: &[u8],
) -> Option<(i32, i32)> {
    let info = bytes.strip_prefix(b"fLaC")?.get(4..)?;
    let packed = u64::from_be_bytes(
        info.get(10..18)?.try_into().ok()?,
    );
    let sample_rate = i32::try_from(packed >> 44).ok()?;
    let bits =
        i32::try_from((packed >> 36) & 0x1f).ok()? + 1;
    Some((bits, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for tier in Tier::ALL {
            assert_eq!(
                Tier::from_name(tier.name()),
                Some(tier)
            );
        }
        assert_eq!(Tier::from_name("exhigh"), None);
    }

    #[test]
    fn only_lossless_and_above_count_as_lossless() {
        assert!(!Tier::High.is_lossless());
        assert!(Tier::Lossless.is_lossless());
        assert!(Tier::HiRes.is_lossless());
        assert_eq!(Some(&Tier::HIGHEST), Tier::ALL.last());
    }

    #[test]
    fn a_missing_tier_is_read_off_format_and_bit_rate() {
        assert_eq!(
            guess_tier("FLAC", 900_000),
            Tier::Lossless
        );
        assert_eq!(guess_tier("mp3", 320_000), Tier::High);
        assert_eq!(
            guess_tier("mp3", 192_000),
            Tier::Standard
        );
        assert_eq!(guess_tier("mp3", 128_000), Tier::Low);
    }

    /// 一段最小的 FLAC 头:STREAMINFO 里给定采样率、双声道、给定位深。
    fn flac_header(sample_rate: u64, bits: u64) -> Vec<u8> {
        let mut bytes = b"fLaC".to_vec();
        bytes.extend([0x80, 0, 0, 34]);
        bytes.extend([0; 10]);
        let packed = (sample_rate << 44)
            | (1 << 41)
            | ((bits - 1) << 36);
        bytes.extend(packed.to_be_bytes());
        bytes.extend([0; 16]);
        bytes
    }

    #[test]
    fn flac_stream_info_reads_depth_and_rate() {
        assert_eq!(
            flac_stream_info(&flac_header(44_100, 16)),
            Some((16, 44_100))
        );
        assert_eq!(
            flac_stream_info(&flac_header(96_000, 24)),
            Some((24, 96_000))
        );
    }

    #[test]
    fn flac_stream_info_rejects_other_bytes() {
        assert_eq!(flac_stream_info(b"ID3\x04"), None);
        assert_eq!(flac_stream_info(b"fLaC\x80\0\0"), None);
    }
}
