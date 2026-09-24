# synctest/src

- `main.rs`:命令行入口(`media` / `serve` / `play` / `record`)与每秒一行的状态日志。
- `clock.rs`:单调时钟、UDP 消息、校时(一簇往返取最小 RTT,线性拟合跟踪漂移)。
- `timeline.rs`:计划(分段表)与跟随器 —— 给定一块音频的呈现时刻,决定跳还是微调速率。纯逻辑,带测试。
- `player.rs`:两个后端共用的出声核心 —— 按这一块的呈现时刻查计划、驱动跟随器、填音频、记状态。
- `media.rs`:固定测试媒体的生成与身份(FNV-1a 64)。
- `wav.rs`:16 位 PCM WAV 的读写,够用即可。
- `out_linux.rs`:电脑侧出声(cpal),呈现时刻取自回调里的 playback − callback。
- `aaudio.rs`:小米侧出声与录音(NDK AAudio),呈现时刻取自 `AAudioStream_getTimestamp`。
