//! 播放页可视化的数据源:把播出路径上分来的 PCM 变成一帧帧频谱与波形。
//!
//! 挖点在 [`crate::Player::play`]:每换一路源就用 [`crate::codec::Tee`] 分一支
//! 采样接到这里,单机、主控、听众三种角色因此天然一致(设计见 `docs/adr/0010`
//! 与 `docs/note/visualization-surface-and-audio.md`)。频谱不进网络、不进契约。
//!
//! 布局照抄 Shadertoy 的音频纹理约定:512 点频谱 + 512 点波形,两行 u8。
//! 这样 Shadertoy 上所有音频响应 shader 的采样代码可以原样搬过来用。
//!
//! FFT 在 CPU 上做(rustfft,2048 点几十微秒),不上 compute:数据量太小,
//! 往返 GPU 的开销比计算本身还大。原始频谱逐帧抖动剧烈,必须做快起慢落的
//! 包络平滑 —— 这一行是「跟着音乐」和「在抽搐」的分界线。

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// 频谱与波形各自的点数。Shadertoy 音频纹理是 512×2,这里钉死同一布局。
pub const BINS: usize = 512;

/// FFT 窗长(采样)。1024 个正频率 bin,取前 [`BINS`] 个 —— 覆盖到采样率的
/// 四分之一,音乐能量几乎都在这段里,高频半区丢掉反而让谱形更满。
const FFT_SIZE: usize = 2048;

/// 支路容量(采样)。48kHz 立体声下约 85ms:UI 每帧(≈16ms)来取一次,
/// 门关着没人取时任由 `try_send` 丢弃 —— 本机扬声器不为可视化停留。
pub const TAP_CAPACITY: usize = FFT_SIZE * 4;

/// 包络每帧的衰减系数。快起:新值直接顶上去;慢落:旧峰按它逐帧滑下来。
/// 按 ~60fps 的取帧节奏调的值;取帧变慢衰减就变慢,可视化页满帧渲染时不构成问题。
const DECAY: f32 = 0.92;

/// 一帧可视化载荷:row 0 频谱、row 1 波形,各 [`BINS`] 字节,可直接拼成
/// 512×2 的 R8 纹理上传。u8 量化在这里做掉,让 GPU 侧零转换。
pub struct VizFrame {
    /// 频谱行:第 i 字节是第 i 个 FFT bin 的包络幅度,开方压缩后量化到 0..=255。
    pub spectrum: [u8; BINS],
    /// 波形行:最近 [`BINS`] 个单声道采样,[-1,1] 线性映到 0..=255,静音在中位。
    pub waveform: [u8; BINS],
}

/// 频谱分析器:audio 输出线程往支路灌采样,UI 线程每帧 [`Self::frame`] 取一份。
///
/// 可克隆的共享句柄([`crate::Player`] 持有一份、UI 侧持有一份)。锁内只有
/// 环形缓冲与 FFT 状态,音频线程从不碰锁 —— 它只往 `Tee` 的支路 `try_send`。
#[derive(Clone)]
pub struct Analyzer(Arc<Mutex<State>>);

/// 分析器内部状态。支路接收端随每次换源被 [`Analyzer::attach`] 替换。
struct State {
    /// 当前源的支路。`None` 表示还没放过任何东西。
    rx: Option<mpsc::Receiver<f32>>,
    /// 当前源的声道数,交错采样按它折成单声道。
    channels: u16,
    /// 折声道的中间量:当前帧已累加的和与已收的声道序号。
    chan_acc: f32,
    chan_pos: u16,
    /// 最近 [`FFT_SIZE`] 个单声道采样,预填静音 —— 没声音时波形就是一条中线。
    ring: VecDeque<f32>,
    /// 各 bin 的包络(线性幅度),快起慢落。
    env: [f32; BINS],
    /// 预排的 FFT 与复用的输入缓冲。
    fft: Arc<dyn Fft<f32>>,
    buf: Vec<Complex<f32>>,
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    /// 建一个还没接任何源的分析器,取帧得到整帧静音。
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            rx: None,
            channels: 1,
            chan_acc: 0.0,
            chan_pos: 0,
            ring: VecDeque::from(vec![0.0; FFT_SIZE]),
            env: [0.0; BINS],
            fft: FftPlanner::new()
                .plan_fft_forward(FFT_SIZE),
            buf: vec![Complex::default(); FFT_SIZE],
        })))
    }

    /// 接上一路新源的支路。旧支路直接丢弃 —— 换歌即换谱。
    ///
    /// `channels` 是**边界输入**(来自解码器探测),0 按 1 处理,不 panic。
    pub fn attach(
        &self,
        rx: mpsc::Receiver<f32>,
        channels: u16,
    ) {
        let mut s =
            self.0.lock().expect("analyzer 锁不该中毒");
        s.rx = Some(rx);
        s.channels = channels.max(1);
        s.chan_acc = 0.0;
        s.chan_pos = 0;
    }

    /// 取一帧:排干支路里攒下的采样、折单声道进环,再做 FFT 与包络。
    ///
    /// 每次调用衰减一次包络,由可视化的帧驱动定节奏 —— 门关着就没人调,
    /// 画面定格,包络也原地冻结,重开门时从冻结处继续滑落。
    pub fn frame(&self) -> VizFrame {
        let mut s =
            self.0.lock().expect("analyzer 锁不该中毒");
        s.drain();

        // 加 Hann 窗抑制频谱泄漏,归一化取 A·N/4(N/2 幅度 × 窗的 0.5 相干增益),
        // 让满幅正弦的峰值恰好压在 1.0。
        let st = &mut *s;
        let (ring, buf) = (&st.ring, &mut st.buf);
        for (i, (slot, sample)) in
            buf.iter_mut().zip(ring.iter()).enumerate()
        {
            let hann = 0.5
                - 0.5
                    * (core::f32::consts::TAU * i as f32
                        / FFT_SIZE as f32)
                        .cos();
            *slot = Complex::new(sample * hann, 0.0);
        }
        let fft = s.fft.clone();
        fft.process(&mut s.buf);

        let norm = 4.0 / FFT_SIZE as f32;
        let mut spectrum = [0u8; BINS];
        for (i, out) in spectrum.iter_mut().enumerate() {
            let raw = s.buf[i].norm() * norm;
            // 快起慢落:新值瞬间顶上去,没有新值就按 DECAY 滑下来。
            s.env[i] = raw.max(s.env[i] * DECAY);
            // 开方压缩再量化:低幅细节抬起来,不然谱形只剩几根孤峰。
            *out = (s.env[i].clamp(0.0, 1.0).sqrt() * 255.0)
                .round() as u8;
        }

        let mut waveform = [0u8; BINS];
        for (out, sample) in waveform
            .iter_mut()
            .zip(s.ring.iter().skip(FFT_SIZE - BINS))
        {
            *out = ((sample.clamp(-1.0, 1.0) * 0.5 + 0.5)
                * 255.0)
                .round() as u8;
        }

        VizFrame { spectrum, waveform }
    }
}

impl State {
    /// 把支路里已到的采样全部收进环:交错采样按声道数折成单声道。
    fn drain(&mut self) {
        let Some(rx) = &self.rx else { return };
        let channels = f32::from(self.channels);
        // try_iter 只收已到的,永不阻塞 —— 这里是 UI 线程。
        while let Ok(sample) = rx.try_recv() {
            self.chan_acc += sample;
            self.chan_pos += 1;
            if self.chan_pos >= self.channels {
                let mono = self.chan_acc / channels;
                self.ring.pop_front();
                self.ring.push_back(mono);
                self.chan_acc = 0.0;
                self.chan_pos = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use similar_asserts::assert_eq;

    use super::*;

    /// 建一个已接好支路的分析器,返回它与灌采样用的发送端。
    fn analyzer(
        channels: u16,
    ) -> (Analyzer, mpsc::SyncSender<f32>) {
        let (tx, rx) = mpsc::sync_channel(FFT_SIZE * 8);
        let a = Analyzer::new();
        a.attach(rx, channels);
        (a, tx)
    }

    /// 灌入正好落在第 `bin` 个 FFT bin 上的正弦(整数周期,无泄漏),`len` 个单声道采样。
    fn send_sine(
        tx: &mpsc::SyncSender<f32>,
        amplitude: f32,
        bin: usize,
        len: usize,
    ) {
        let step = core::f32::consts::TAU * bin as f32
            / FFT_SIZE as f32;
        for i in 0..len {
            tx.send(amplitude * (step * i as f32).sin())
                .expect("测试支路不该满");
        }
    }

    /// 输出布局钉死 512 频谱 + 512 波形(Shadertoy 音频纹理约定),行序不可换。
    #[test]
    fn frame_layout_is_512_spectrum_plus_512_waveform() {
        let (a, tx) = analyzer(1);
        send_sine(&tx, 0.8, 48, FFT_SIZE);
        let f = a.frame();
        assert_eq!(BINS, 512);
        assert_eq!(f.spectrum.len(), BINS);
        assert_eq!(f.waveform.len(), BINS);
    }

    /// 喂第 48 号 bin 的纯音,频谱峰值必须落在 48 附近 ——
    /// 「跟得上鼓点」的最小可断言形式。
    #[test]
    fn pure_tone_peaks_at_expected_bin() {
        let (a, tx) = analyzer(1);
        send_sine(&tx, 0.8, 48, FFT_SIZE);
        let f = a.frame();
        let peak = f
            .spectrum
            .iter()
            .enumerate()
            .max_by_key(|(_, v)| **v)
            .expect("频谱行非空")
            .0;
        assert!(
            (47..=49).contains(&peak),
            "峰值应在 bin 48 附近,实落 {peak}"
        );
    }

    /// 包络快起慢落:首帧立即顶到高位;静音后逐帧单调滑落,最终归零而不是跳零。
    #[test]
    fn envelope_attacks_instantly_and_decays_slowly() {
        let (a, tx) = analyzer(1);
        send_sine(&tx, 0.9, 48, FFT_SIZE);
        let first = a.frame().spectrum[48];
        assert!(
            first >= 180,
            "首帧应立即顶起(快起),实得 {first}"
        );

        // 静音把环冲净,此后每帧只剩包络自身在衰减。
        for _ in 0..FFT_SIZE {
            tx.send(0.0).expect("测试支路不该满");
        }
        let mut prev = a.frame().spectrum[48];
        assert!(
            prev > 0,
            "静音后的第一帧应保留衰减中的包络(慢落)"
        );
        for _ in 0..250 {
            let cur = a.frame().spectrum[48];
            assert!(
                cur <= prev,
                "衰减必须单调:{cur} > {prev}"
            );
            prev = cur;
        }
        assert_eq!(prev, 0, "衰减最终应归零");
    }

    /// 立体声左右同相交错采样折成单声道后,峰值幅度与单声道一致。
    #[test]
    fn stereo_interleaved_folds_to_mono() {
        let (mono, tx_m) = analyzer(1);
        send_sine(&tx_m, 0.8, 48, FFT_SIZE);
        let m = mono.frame().spectrum[48];

        let (stereo, tx_s) = analyzer(2);
        let step =
            core::f32::consts::TAU * 48.0 / FFT_SIZE as f32;
        for i in 0..FFT_SIZE {
            let v = 0.8 * (step * i as f32).sin();
            tx_s.send(v).expect("测试支路不该满"); // 左
            tx_s.send(v).expect("测试支路不该满"); // 右
        }
        let s = stereo.frame().spectrum[48];
        assert!(
            (i16::from(m) - i16::from(s)).abs() <= 2,
            "单声道 {m} 与立体声 {s} 应相当"
        );
    }

    /// 静音输入:频谱行全零,波形行齐平在中位 —— 数值形态钉死,NaN 无处藏身。
    #[test]
    fn silence_yields_midlevel_waveform_and_zero_spectrum()
    {
        let (a, tx) = analyzer(1);
        for _ in 0..FFT_SIZE {
            tx.send(0.0).expect("测试支路不该满");
        }
        let f = a.frame();
        assert!(f.spectrum.iter().all(|v| *v == 0));
        assert!(
            f.waveform
                .iter()
                .all(|v| (126..=129).contains(v))
        );
    }

    /// 一颗采样都没喂(连 attach 都没有)就取帧:给整帧静音,不 panic ——
    /// 播放页可能先于任何声音被展开。
    #[test]
    fn frame_before_any_samples_is_silent_not_panic() {
        let a = Analyzer::new();
        let f = a.frame();
        assert!(f.spectrum.iter().all(|v| *v == 0));
        assert!(
            f.waveform
                .iter()
                .all(|v| (126..=129).contains(v))
        );
    }
}
