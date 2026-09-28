# 音频模型打标 spike（#162）

问题：网易云不给风格、乐器种类和乐器数量，能不能靠解析音频补上；准的话接 worker 写回 `tags`
（`source=model`，#158），不准就换路。解析不能进生产 server（512Mi 内存限制），只在 pc3 上跑。

## 1. 候选模型与许可证

| 候选 | 能出什么 | 许可证 | 取舍 |
|---|---|---|---|
| Essentia EffNet-Discogs 嵌入 + 分类头 | Discogs 400 类风格（15 大类下的细分）、MTG-Jamendo 40 类乐器 / 87 类风格 / 56 类情绪主题 | 模型 **CC BY-NC-SA 4.0**（MTG 另可商谈专有许可）；Essentia 库 AGPL-3.0 | 有监督训练，标签体系固定；模型小（嵌入 18MB，每个头 2–3MB），CPU 就够 |
| LAION-CLAP `music_audioset_epoch_15_esc_90.14.pt` | 零样本：任意英文提示词和音频比相似度，风格、乐器、编制都能问 | 代码与权重仓库 **CC0-1.0** | 标签表可以按用户曲库自定；权重 2.35GB，要 torch，GPU 上快 |
| 乐器数量 | 两者都没有直接输出 | — | 只能从乐器概率数「过阈值的乐器族」，或用 CLAP 问「独奏 / 二重奏 / 小编制 / 乐队 / 管弦乐」 |

出处：Essentia 模型页 <https://essentia.upf.edu/models.html>（许可证原文：“All the models created by
the MTG are licensed under CC BY-NC-SA 4.0”）；CLAP 仓库 <https://github.com/LAION-AI/CLAP>（LICENSE 为
CC0-1.0，音乐 checkpoint 在 `huggingface.co/lukewys/laion_clap`）。

许可证上要用户知道的一条：Essentia 模型是**非商业**许可。osmosis 目前是自用，不冲突；将来若收费或
对外提供服务，要么换 CLAP 这类 CC0 的，要么向 MTG 要专有许可。AGPL 只约束分发和对外服务，worker
在 pc3 内部跑、只往自己的库里写标签，不触发。

这次两条都跑：Essentia 用 Discogs400、Jamendo 乐器、Jamendo 风格、Jamendo 情绪四个头；CLAP 用
自定的 20 个风格词、18 个乐器词、5 档编制。
