# audio

音频播放能力层:把一条直链变成声音,并吸收各端音频后端的差异(linux 走 alsa、
android 走 AAudio、web 将来走 WebAudio)。与 `api`、`render3d` 平行,`app-core`
不认识本 crate,由 `ui` 注入。边界:只管声音的取得、解码、播出与派生数据
(同播支路、可视化频谱),不碰 UI、不碰网络协议的形状。

## 文件

- `src/lib.rs`:crate 门面。`load` 把直链变成边下边播的流(内部自带多线程
  tokio runtime,解码的阻塞读与下载必须同 runtime 不同线程);`decode` 是
  纯解码入口,测试与生产走同一条路径;`Player` 持有音频设备并承担播出,
  它的 `play` 同时是同播 tee 与可视化分析器的统一挖点。起播前按
  `PREFETCH_BYTES` 攒一段,让开头那几秒不必指望网络准时;曲中掉速则由
  `stream_source::buffered` 兜住。
- `src/codec.rs`:同播用的 Opus 编解码与 `Tee`。`normalize` 把任意源统一成
  48kHz 立体声,`Tee` 把播放中的采样原样传下去、复制一份进有界支路,
  攒帧逻辑吸收「Opus 只收固定帧长而 rodio 一次给一个采样」的错位。
- `src/spectrum.rs`:播放页可视化的数据源。接 `Tee` 支路收 PCM,折单声道进
  环形缓冲,rustfft 2048 点 FFT 加快起慢落包络,产出 Shadertoy 音频纹理布局
  (512 频谱 + 512 波形,u8 两行)的 `VizFrame`。只产数据,不碰 GPU。
- `src/stream_source.rs`:`ChannelSource` 与 `buffered`。前者把一条采样通道
  包装成 rodio 源,没数据时给静音而不是结束 —— 断流等于无声,不等于播完;
  听众侧直接用它接网络推来的 PCM。后者把本机播放的解码挪到自己的线程上,
  隔着有界通道喂前者:rodio 在声卡回调里直接要采样,不隔这一层的话,
  流式解码器一次阻塞读就是一次设备欠载。
- `tests/`:联机测试(`--ignored`),自己打 axum 后端拿真实直链验证整条
  下载解码链。不走 `api` crate,免得两个平行能力层串成依赖链。
