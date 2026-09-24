//! 固定测试媒体:两个声道各一路每秒一次的扫频标记,频段错开,录音里分得开。

pub const RATE: u32 = 48_000;
/// 标记间隔。
pub const PERIOD_S: f64 = 1.0;
/// 一个扫频多长。
pub const CHIRP_S: f64 = 0.030;
/// 左声道(电脑)与右声道(小米)的扫频频段,Hz。`analyze.py` 里抄着同样的数。
pub const BAND_L: (f64, f64) = (1_000.0, 2_500.0);
pub const BAND_R: (f64, f64) = (4_000.0, 7_000.0);

/// 一个带 Hann 窗的线性扫频。
pub fn chirp(f0: f64, f1: f64) -> Vec<f32> {
    let n = (CHIRP_S * f64::from(RATE)) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / f64::from(RATE);
            let k = (f1 - f0) / CHIRP_S;
            let phase = 2.0 * std::f64::consts::PI * (f0 * t + 0.5 * k * t * t);
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
            (phase.sin() * w * 0.8) as f32
        })
        .collect()
}

/// 生成 `secs` 秒的双声道交错样本。标记落在每秒的 0.5 秒处:起播那一刻不带标记,
/// 免得第一个标记正好压在「还在对齐」的那几毫秒上。
pub fn generate(secs: u32) -> Vec<i16> {
    let frames = (secs * RATE) as usize;
    let mut out = vec![0i16; frames * 2];
    let (l, r) = (chirp(BAND_L.0, BAND_L.1), chirp(BAND_R.0, BAND_R.1));
    let mut k = 0.5;
    while ((k + CHIRP_S) * f64::from(RATE)) < frames as f64 {
        let start = (k * f64::from(RATE)) as usize;
        for (i, (&a, &b)) in l.iter().zip(&r).enumerate() {
            out[(start + i) * 2] = (a * 32_767.0) as i16;
            out[(start + i) * 2 + 1] = (b * 32_767.0) as i16;
        }
        k += PERIOD_S;
    }
    out
}

/// 媒体身份:两端各自算,对不上就不放。FNV-1a 64,够用。
pub fn identity(samples: &[i16]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in samples {
        for b in s.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// 取出一个声道,转成 f32。
pub fn channel(samples: &[i16], channels: usize, which: usize) -> Vec<f32> {
    samples
        .chunks_exact(channels)
        .map(|f| f32::from(f[which]) / 32_768.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 标记在每秒 0.5 秒处,两个声道同一时刻起。
    #[test]
    fn markers_sit_at_half_seconds_on_both_channels() {
        let s = generate(3);
        let l = channel(&s, 2, 0);
        let r = channel(&s, 2, 1);
        let first = |c: &[f32]| c.iter().position(|v| v.abs() > 1e-4).unwrap();
        let at = (0.5 * f64::from(RATE)) as usize;
        assert!(first(&l).abs_diff(at) < 10);
        assert!(first(&r).abs_diff(at) < 10);
    }

    /// 同样的输入同样的身份,改一个样本就变。
    #[test]
    fn identity_changes_with_the_content() {
        let mut s = generate(1);
        let a = identity(&s);
        assert_eq!(a, identity(&generate(1)));
        s[100] ^= 1;
        assert_ne!(a, identity(&s));
    }
}
