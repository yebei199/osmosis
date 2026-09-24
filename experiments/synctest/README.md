# synctest

#137 ② 的紧同步技术验证:两台设备**各自**播同一份测试音频,靠共享时间轴对齐,
用小米 13 的麦克风录下两路真实出声,算两两偏差。只回答「这条路能对齐到多少」,
不是产品代码(见 `../README.md`)。

## 它做什么

- `synctest media`:生成固定测试媒体 `synctest.wav`(48kHz 双声道)。左声道每秒一个
  1–2.5kHz 扫频,右声道同一时刻一个 4–7kHz 扫频。两端放同一份文件、各放一个声道,
  录音里按频段分得开是谁的声音。
- `synctest serve`(电脑):时间服务器(UDP 往返)+ 发布计划(起播时刻、seek)+ 自己按计划出声(cpal)。
- `synctest play`(小米,adb shell 直接跑):多次往返估时钟偏移与漂移,按计划出声(AAudio)。
- `synctest record`(小米):麦克风录音到 WAV(AAudio 输入,`Unprocessed` 预设)。
- `analyze.py`:匹配滤波找两路扫频的到达时刻,扣声程,出偏差统计。
- `run.sh`:一键跑一轮(推文件、起服务、录、拉回、分析)。

## 对齐怎么做

1. 校时:客户端每秒发一簇 UDP ping,取 RTT 最小的那次,偏移 = ((t1−t0)+(t2−t3))/2,
   不确定度 = RTT/2;最近若干次做线性拟合跟踪漂移。不改系统时钟,也不直接相减两台机器的读数。
2. 计划:服务端用自己的单调时钟发「在 T 时刻媒体位置应是 P」的分段表,客户端换算到本机时钟。
3. 出声:每个音频回调算出这一块**第一帧的实际呈现时刻**(AAudio 用 `getTimestamp`,cpal 用
   `OutputCallbackInfo` 的 playback−callback 差),对照计划得到应在的媒体位置。误差超过 10ms 直接
   跳过去(起播、seek、重连);小于 10ms 用 ±0.1% 以内的速率微调慢慢追,不靠反复 seek。

## 依赖

电脑侧要 ALSA(`nix-shell ../../slint.nix`),安卓侧用 NDK 交叉编译(`nix-shell ../../Android.nix`),
分析要 python3 + numpy + scipy。
