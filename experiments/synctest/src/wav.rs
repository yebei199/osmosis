//! 16 位 PCM WAV 的读写。只认本实验自己写出来的格式,够用即可。

use std::io::{self, Read, Write};

pub fn header(channels: u16, rate: u32, frames: u32) -> Vec<u8> {
    let data_len = frames * u32::from(channels) * 2;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    h.extend_from_slice(&(channels * 2).to_le_bytes());
    h.extend_from_slice(&16u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

pub fn write(path: &str, channels: u16, rate: u32, samples: &[i16]) -> io::Result<()> {
    let frames = (samples.len() / usize::from(channels)) as u32;
    let mut f = io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(&header(channels, rate, frames))?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

/// 读回 (声道数, 采样率, 交错样本)。
pub fn read(path: &str) -> io::Result<(u16, u32, Vec<i16>)> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "不是本实验写的 16 位 PCM WAV");
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[36..40] != b"data" {
        return Err(bad());
    }
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let samples = bytes[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok((channels, rate, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写出去再读回来,一个样本不差。
    #[test]
    fn a_written_file_reads_back() {
        let dir = std::env::temp_dir().join(format!("synctest-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let path = path.to_str().unwrap();
        let samples = vec![0i16, 1, -1, i16::MAX, i16::MIN, 42];
        write(path, 2, 48_000, &samples).unwrap();
        assert_eq!(read(path).unwrap(), (2, 48_000, samples));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
