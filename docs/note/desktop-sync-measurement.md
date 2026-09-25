# 桌面多实例同步的静音测量

在一台机器上起两三个桌面实例组成播放组，声音各进一个 PipeWire null sink,用同一条录音流把
几个 sink 的 monitor 一起录下来，逐秒互相关，量出实例之间的相对偏差。不出可闻声音，也不改默认
输出。#137 ⑤ 起用这套办法，#138 拿它复现 xrun 之后的偏差。

## 搭起来

- **null sink**:`pw-cli create-node adapter '{ factory.name=support.null-audio-sink
  node.name=osmosis_null_1 media.class=Audio/Sink object.linger=true audio.position=[FL FR]
  audio.rate=48000 priority.session=1 priority.driver=1 }'`,优先级压到 1,默认输出不会被它抢走。
  建完用 `wpctl inspect @DEFAULT_AUDIO_SINK@ | grep node.name` 核一遍，收尾时用 `pw-cli destroy <id>` 删掉。
- **显示**:`Xvfb :7`,实例带 `DISPLAY=:7`、去掉 `WAYLAND_DISPLAY`。
- **每个实例一个 net ns**:单实例锁是 abstract socket,按 net ns 隔开，所以第二个实例起不来
  (「已经有一个 osmosis-desktop 在跑了」)。用 pasta 起：
  `pasta --config-net -f --host-lo-to-ns-lo -t 809<n>:8091 -T 3000,8765 -- unshare -m sh -c
  'mount --bind <文件> /etc/hostname && exec <应用>'`。在 ns 里各实例都叫 `#1`,名册上靠换掉的主机名
  (`pc1d<n>`)区分。应用的环境:`PIPEWIRE_NODE=osmosis_null_<n>`、各自的 `XDG_STATE_HOME`(登录态和
  设备 id 都在里面)、`SLINT_MCP_PORT=8091`,再去掉代理变量。
- **媒体**:debug 包认状态目录里的 `osmosis-dev/test-media-url`(`api::test_media_url`),写上
  `http://127.0.0.1:8765/L/{duration_ms}.wav`,再起一个按这个路径现生成带标记音频的 HTTP 服务。
  标记是每秒一个 30ms 的扫频，互相关锁得住。
- **组组**:经 MCP 在主端的「更多」抽屉里按「加入 pc1d<n> #1」。

## 量

`pw-record --target 0 -P '{ node.name=osmo_rec node.autoconnect=false }' --format s16 --rate 48000
--channels 3 --channel-map FL,FR,FC all.wav`,再用 `pw-link osmosis_null_<n>:monitor_FL
osmo_rec:input_<FL|FR|FC>` 把每个 sink 接到一个声道上。几路共用一个录音流，时基就是同一个。拆开后
按 1 秒窗口互相关，取 ±400ms 内的峰值，正数表示后一路晚。

冻结一个实例:`kill -STOP <pid>; sleep 4; kill -CONT <pid>`,pid 按 `/proc/<pid>/environ` 里的
`XDG_STATE_HOME` 认，不按进程名批量杀。冻结前录 8 秒，恢复后录 20 秒。

## #138 的结论:xrun 之后的稳定偏差在当前 master 上复现不出来

#137 sc5b 那一轮报过冻结恢复后 d2 稳在 −10.5ms、d3 在 −5.5ms。2026-09-25 用上面这套办法冻了 14 次
(两实例、三实例全 pasta;冻跟随端、冻主端;4s 和 8s),每次恢复后前几秒有几 ms 的跳变，也就是
xrun 期间断掉的那一截，2 到 10 秒内被跟随器追回，最后 4 秒全在 ±0.05ms 以内。pasta 里的实例拿不到
实时调度，每 30 秒左右还会自己 xrun 一次，这些也都追回来了。回头看 sc5b 那组数据,d3 在冻结之前
就已经是 −5.48ms(冻的是 d2),不像是 xrun 造成的。

插件这一层(pipewire-alsa 1.6.8 的 `pcm_pipewire.c`,对照 `PIPEWIRE_DEBUG=alsa.pcm:T` 的 trace)
的情况如下:

- cpal 在 ALSA 上只要设备支持就开 44100Hz,而图跑在 48k。插件每周期的 `want` 因此在 470/471
  之间来回跳。
- SIGCONT 之后，同一个图周期里 process 被调了两次，环形缓冲被抽空，报 XRUN,cpal 随即
  `prepare`。
- 可疑点:`snd_pcm_pipewire_prepare` 不清 `transferred`/`buffered`,delay 里会带上一份
  `transferred % want` 的余数。只要 want 在两个值之间跳，这份余数一两个周期就自己清零。trace 里恢复后
  delay 的 `filled` 一项(约 465)和冻结前一样。

**如果再看到稳定偏差**，先把这几样现场留下来再改代码:宿主上还有哪些音频客户端
(`pw-cli ls Node`)、图的 quantum 和采样率(`pw-top`)、cpal 实际开流的采样率与周期(trace 里的
`RATE:`、`PERIOD_SIZE:`)是不是和图的采样率不同、偏差出现之前那几秒的 `alsa.pcm` trace。want 恒定
的配置(开流采样率等于图采样率)下，上面那份余数不会自清，是头号嫌疑。
