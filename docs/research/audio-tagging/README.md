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

## 2. pc3 上 50 首打标

### 抽样

从生产库「我喜欢」（2026-09-28 共 980 首）按 `pick.py` 抽：按第一艺人计数，前 15 位艺人各取最近喜欢
的 2 首（30 首，覆盖常听的几类：新古典钢琴、澤野弘之 / 横山克的动画配乐、J-rock、动画歌、lo-fi、
坂本龍一），其余曲目用 `random.Random(162)` 抽 20 首（长尾：phonk、post-rock、trap、俄语说唱、
弦乐四重奏翻奏……）。50 首全部拿到整曲，没有 30 秒试听。

### 怎么跑

音频走生产 server 的 `GET /play/{id}` 拿播放源地址直接下载（`fetch.sh`，42 首 flac、8 首 mp3，共
1.37GB），只留在 pc3 的工作目录里，打完删掉。两套模型分开跑：

- `tag_essentia.py`：16kHz 单声道整曲，EffNet-Discogs 嵌入后接四个头，按帧平均取概率。
- `tag_clap.py`：48kHz，取曲目 25% / 50% / 75% 处各 10 秒，音频嵌入平均后和文本提示比余弦相似度。

复现：pc3 上建 `~/audiotag`，`uv venv -p 3.11` 后装 `essentia-tensorflow laion-clap torch==2.11.0
torchvision==0.26.0`（pc3 走代理下 PyPI 很慢，去掉代理变量、用 `mirrors.aliyun.com` 的 PyPI 镜像）；
模型放 `models/`，Essentia 的从 `essentia.upf.edu/models/` 下（要走代理），CLAP 权重从
`hf-mirror.com/lukewys/laion_clap` 下（不走代理，sha256 `fae3e9c0…6dedd` 与官方一致）；`token` 是
`POST /login` 拿到的会话令牌。环境变量要带 `LD_LIBRARY_PATH=/run/opengl-driver/lib:/run/current-system/sw/share/nix-ld/lib`。
表由 `table.py` 从三份 JSON 生成。

### 吞吐（实测，2026-09-28，pc3：RTX 5080 16GB、28 线程）

| 阶段 | 设备 | 每首 | 备注 |
|---|---|---|---|
| 下载 | 网络 | 约 8.4s | 平均 27.4MB / 首（最高音质多是 flac），抽测 8 首下载速度 1.2–5.0MB/s，均值约 3.3MB/s |
| Essentia 解码 + 重采样 | CPU | 0.24s | |
| Essentia 推理（嵌入 + 4 头） | CPU，8 线程，`nice 19` | 1.24s（最长 2.24s） | 平均曲长 245s，约 197 倍实时；50 首连加载共 75s |
| CLAP 解码 + 重采样 | CPU | 0.31s | |
| CLAP 推理（3×10s） | GPU | 0.017s | 模型加载一次约 1 分钟，50 首连加载共 129s |

Essentia 跑不上这块 GPU：`essentia-tensorflow` 带的 TensorFlow 链的是 CUDA 11 / cuDNN 8
（日志 `Could not load dynamic library 'libcudnn.so.8'`），而且那一代 TF 不认 RTX 5080 的
compute capability 12.0。它在 CPU 上已经够快，不值得去折腾。

全曲库估算：以「我喜欢」980 首计，单线程串行下载约 2.3 小时，Essentia 约 24 分钟，CLAP 约 5 分钟。
瓶颈是下载；worker 若只要 16k / 48k 单声道，改要标准音质（mp3 约 1/3 大小）并发下，可以压到半小时内。

### 50 首的标签表

末三栏留给用户判：**E** = Essentia 那边对，**C** = CLAP 那边对，**两** = 都对，**无** = 都不对。
「风格」看前三列（Discogs、Jamendo 算 Essentia），「乐器」看 Essentia 乐器族与 CLAP 乐器，「数量」
看倒数第四列（左 Essentia 数出的乐器族数，右 CLAP 的编制判断）。

| # | 曲目 | Discogs 风格 top3 | Jamendo 风格 top2 | CLAP 风格 top2 | Essentia 乐器(族,p≥0.2) | CLAP 乐器 top3 | 数量:Essentia / CLAP | 风格 | 乐器 | 数量 |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | Atlas — 小瀬村晶 | Ambient, Neo-Classical, Contemporary | classical, soundtrack | neoclassical piano, ambient | piano | piano, harp, choir | 1 / 二重奏 |  |  |  |
| 2 | You and Me — 小瀬村晶 | Contemporary, Soundtrack, Neo-Classical | classical, soundtrack | neoclassical piano, epic orchestral film score | piano | piano, orchestra, choir | 1 / 二重奏 |  |  |  |
| 3 | Blue Dragon(piano&guitarver) — 澤野弘之 | Folk, Ambient, Celtic | classical, soundtrack | folk, epic orchestral film score | piano | harp, acoustic guitar, cello | 1 / 独奏 |  |  |  |
| 4 | LINK03BPM71THEME — 澤野弘之 | Gothic Metal, Darkwave, Ambient | electronic, soundtrack | epic orchestral film score, cinematic | synth, guitar, piano, drums | orchestra, choir, string ensemble | 4 / 乐队 |  |  |  |
| 5 | Drop Variation — Ludovico Einaudi | Contemporary, Neo-Classical, Post-Modern | classical, soundtrack | neoclassical piano, epic orchestral film score | piano | piano, harp, acoustic guitar | 1 / 二重奏 |  |  |  |
| 6 | Pathos — Ludovico Einaudi | Contemporary, Neo-Classical, Soundtrack | soundtrack, classical | epic orchestral film score, classical string quartet | piano, strings | orchestra, cello, violin | 2 / 乐队 |  |  |  |
| 7 | Represent feat. Kimbara Chieko — DJ OKAWARI | Ambient, Downtempo, Synth-pop | pop, electronic | j-pop, cinematic | piano, guitar, synth | violin, cello, harp | 3 / 独奏 |  |  |  |
| 8 | Eventually — DJ OKAWARI / Emily Styler | Downtempo, K-pop, Ambient | hiphop, electronic | j-pop, anime soundtrack | synth, piano, drums, bass | electronic beats, choir, vocals | 4 / 乐队 |  |  |  |
| 9 | Old City — Mili | IDM, Experimental, Glitch | electronic, ambient | anime soundtrack, anime song | piano, synth | electronic beats, piano, choir | 2 / 乐队 |  |  |  |
| 10 | In Hell We Live, Lament (Let's Lament) — Mili / KIHOW | Contemporary, Ballad, Vocal | classical, pop | anime soundtrack, j-pop | piano | piano, choir, cello | 1 / 乐队 |  |  |  |
| 11 | like there is tomorrow — TK from 凛として時雨 | Alternative Rock, Indie Rock, Ballad | pop, rock | post-rock, anime soundtrack | piano, drums, bass, guitar | piano, electric guitar, choir | 4 / 独奏 |  |  |  |
| 12 | unravel (acoustic version) — TK from 凛として時雨 | K-pop, Ballad, Alternative Rock | pop, rock | j-pop, anime soundtrack | piano, bass, guitar, drums | piano, vocals, electric guitar | 4 / 独奏 |  |  |  |
| 13 | βios — 小林未郁 | Symphonic Rock, Heavy Metal, Gothic Metal | rock, electronic | pop, anime soundtrack | guitar, synth, drums, bass | vocals, choir, electric guitar | 4 / 乐队 |  |  |  |
| 14 | ThreeFiveNineFourε — 小林未郁 | Gothic Metal, Symphonic Rock, Heavy Metal | rock, pop | anime soundtrack, anime song | drums, guitar, bass, synth | choir, orchestra, electric guitar | 4 / 乐队 |  |  |  |
| 15 | 望み叶え給え — 横山克 | Ambient, Downtempo, Experimental | ambient, electronic | cinematic, ambient | piano, synth | choir, piano, orchestra | 2 / 乐队 |  |  |  |
| 16 | 身代わり — 横山克 | Ambient, Downtempo, Neo-Classical | soundtrack, ambient | cinematic, epic orchestral film score | piano, synth | orchestra, piano, choir | 2 / 二重奏 |  |  |  |
| 17 | purple clouds — 牛尾憲輔 | Contemporary, Soundtrack, Ambient | classical, soundtrack | epic orchestral film score, chinese traditional music | piano, strings | orchestra, string ensemble, choir | 2 / 二重奏 |  |  |  |
| 18 | sweet dreams — 牛尾憲輔 | Ambient, Experimental, Abstract | ambient, electronic | cinematic, ambient | synth, piano | piano, orchestra, choir | 2 / 乐队 |  |  |  |
| 19 | Departures〜あなたにおくるアイの歌〜 — EGOIST | Ballad, K-pop, J-pop | pop, rock | anime soundtrack, anime song | drums, piano, bass, guitar | piano, electric guitar, bass guitar | 4 / 独奏 |  |  |  |
| 20 | Ghost of a smile (from BEST AL“ALTER EGO”) — EGOIST | K-pop, J-pop, Ballad | pop, popfolk | anime soundtrack, j-pop | piano, guitar | piano, choir, vocals | 2 / 独奏 |  |  |  |
| 21 | 0.vers — 瑞葵(mizuki) | Ballad, Alternative Rock, Gothic Metal | pop, rock | post-rock, anime soundtrack | piano, drums, bass, guitar | choir, piano, orchestra | 4 / 乐队 |  |  |  |
| 22 | aLIEz　[nZk ver.] — 瑞葵(mizuki) / SawanoHiroyuki[nZk] | Contemporary, Vocal, Ballad | classical, pop | anime soundtrack, j-pop | piano | piano, choir, harp | 1 / 独奏 |  |  |  |
| 23 | luz — Haruka Nakamura | Folk, Ambient, Ethereal | ambient, pop | ambient, folk | piano, guitar, synth | choir, piano, harp | 3 / 乐队 |  |  |  |
| 24 | Twilight — Haruka Nakamura | Folk, Ethereal, Neofolk | pop, ambient | folk, anime soundtrack | guitar | choir, harp, vocals | 1 / 乐队 |  |  |  |
| 25 | Happy End — 坂本龍一 / Jaques Morelenbaum / Judy Kang | Modern, Contemporary, Impressionist | classical, soundtrack | neoclassical piano, epic orchestral film score | piano, strings | violin, piano, cello | 2 / 二重奏 |  |  |  |
| 26 | Harry To Hospital — 坂本龍一 | Neo-Classical, Contemporary, Baroque | classical, soundtrack | epic orchestral film score, classical string quartet | piano | orchestra, choir, string ensemble | 1 / 乐队 |  |  |  |
| 27 | for ロンリー — Aimer / 阿部真央 | J-pop, Pop Rock, Indie Rock | pop, rock | anime soundtrack, j-pop | drums, bass, guitar, synth | electric guitar, bass guitar, guzheng | 4 / 乐队 |  |  |  |
| 28 | REMIND YOU — Aimer | Ballad, Alternative Rock, Pop Rock | pop, rock | anime soundtrack, pop | drums, bass, guitar, piano, synth | choir, piano, orchestra | 5 / 乐队 |  |  |  |
| 29 | Snow before Spring — 羽肿 | Ambient, Downtempo, New Age | electronic, chillout | cinematic, ambient | piano, synth | piano, choir, orchestra | 2 / 乐队 |  |  |  |
| 30 | Neighbor's Garden — 羽肿 | Ambient, Downtempo, Ballad | pop, chillout | anime soundtrack, ambient | piano, guitar, drums, bass | piano, drums, choir | 4 / 乐队 |  |  |  |
| 31 | last exit — naan | Indie Rock, Alternative Rock, Folk Rock | rock, pop | folk, post-rock | guitar, drums, bass | orchestra, choir, piano | 3 / 乐队 |  |  |  |
| 32 | 南锣鼓巷 — 接个吻，开一枪 / CLARE | Tropical House, House, Electro House | electronic, house | anime soundtrack, electronic dance music | piano, synth, bass | electronic beats, piano, flute | 3 / 乐队 |  |  |  |
| 33 | Xavii — Russian Circles | Post Rock, Math Rock, Post-Metal | rock, indie | post-rock, j-rock | bass, drums, guitar | bass guitar, piano, orchestra | 3 / 独奏 |  |  |  |
| 34 | The World Retreats — David O'Dowda | Ambient, Experimental, Downtempo | electronic, ambient | ambient, folk | piano, synth, guitar | piano, choir, bass guitar | 3 / 乐队 |  |  |  |
| 35 | 110629 — 宮内優里 | Ambient, Experimental, Downtempo | electronic, ambient | j-pop, folk | piano, drums, guitar, bass, synth | drums, electronic beats, harp | 5 / 二重奏 |  |  |  |
| 36 | Симпл димпл поп ит сквиш — 兮有妹 | House, Techno, Minimal Techno | electronic, house | j-pop, anime song | bass, synth, keys, drums | electronic beats, vocals, electric guitar | 4 / 乐队 |  |  |  |
| 37 | 秋～華恋～ — α·Pav | Ambient, Downtempo, New Age | electronic, soundtrack | post-rock, j-rock | piano, synth, drums | orchestra, choir, piano | 3 / 乐队 |  |  |  |
| 38 | JEWFY SAMPLE CHALLENGE — lil heartbreak | Trap, Cloud Rap, Hardcore Hip-Hop | hiphop, rap | trap, anime song | bass, synth | vocals, electronic beats, choir | 2 / 乐队 |  |  |  |
| 39 | KnocK-on Effect — 遠藤幹雄 | IDM, Experimental, Glitch | electronic, ambient | j-pop, anime soundtrack | piano, synth, drums, bass | piano, electronic beats, choir | 4 / 乐队 |  |  |  |
| 40 | Thecoldtree — arthrn | Experimental, Downtempo, Ambient | electronic, ambient | lo-fi hip hop, r&b | piano | piano, choir, electronic beats | 1 / 乐队 |  |  |  |
| 41 | 前前前世 guitar ver（翻自 RADWIMPS） — 火西肆 | J-pop, Ballad, Dance-pop | pop, electronic | anime soundtrack, anime song | drums, guitar, synth, bass | acoustic guitar, bass guitar, electronic beats | 4 / 独奏 |  |  |  |
| 42 | ninelie (Stereoman Bootleg Remix) — Stereoman / Aimer / EGOIST | Dubstep, K-pop, Glitch | electronic, pop | anime soundtrack, anime song | synth, bass, drums, piano | electronic beats, guzheng, harp | 4 / 乐队 |  |  |  |
| 43 | us — 4oot | Experimental, Ambient, Downtempo | electronic, classical | phonk, j-pop | piano | piano, vocals, electronic beats | 1 / 二重奏 |  |  |  |
| 44 | 世阿弥 — 水曜日のカンパネラ | K-pop, House, Dance-pop | electronic, pop | j-pop, anime soundtrack | synth, bass, drums, piano | electronic beats, violin, electric guitar | 4 / 乐队 |  |  |  |
| 45 | Normal No More — TYSM | K-pop, Tropical House, Dance-pop | pop, electronic | anime soundtrack, j-pop | bass, keys, drums, synth, piano, guitar | electronic beats, electric guitar, bass guitar | 6 / 乐队 |  |  |  |
| 46 | LOVELY BASTARDS (UNNXMED Remix) — ZWE1HVNDXR / yatashigang / UNNXMED | Bassline, Electro House, Dubstep | electronic, dance | electronic dance music, pop | bass, synth, drums, guitar, piano | electronic beats, electric guitar, drums | 5 / 乐队 |  |  |  |
| 47 | Мимими — Dramma | Cloud Rap, Trap, Horrorcore | hiphop, rap | pop, r&b | bass, drums, synth, guitar, piano | electronic beats, bass guitar, electric guitar | 5 / 乐队 |  |  |  |
| 48 | 儚き人の为のカンタータ(off vocal) — はちみつれもん | Downtempo, Chillwave, Ambient | electronic, pop | j-pop, hip hop | synth, guitar, piano, drums | electronic beats, piano, drums | 4 / 乐队 |  |  |  |
| 49 | Blank Space — Vitamin String Quartet | Folk, Nordic, Celtic | classical, folk | classical string quartet, folk | strings, guitar, flute, piano | cello, orchestra, violin | 4 / 独奏 |  |  |  |
| 50 | Exit — A Himitsu | Happy Hardcore, Hardcore, Speedcore | electronic, ambient | anime soundtrack, electronic dance music | synth, piano | electronic beats, orchestra, choir | 2 / 乐队 |  |  |  |

### 用户评分之前就看得出的问题

这几条不靠听，从表里的数字就能看出来，给评分时参考：

- **Essentia 听不出人声。**Jamendo 乐器头的 `voice` 在 50 首里没有一首过 0.2，Aimer、EGOIST、
  TK 这种人声为主的歌也只有 0.07–0.16。所以 Essentia 数的乐器数量不含人声。Essentia 另有专门的
  `voice_instrumental` 分类头，这次没有跑。
- **CLAP 的「编制」基本在乱猜。**50 首里 32 首判「乐队」、10 首判「独奏」，判独奏的里面至少 6 首明显不是独奏：5 首人声歌
  （#11、#12、#19、#20、#22，至少人声加伴奏两件），外加 Russian Circles 的器乐摇滚（#33）；没有一首
  判「小编制」或「管弦乐」。
- **CLAP 的乐器偏向 choir。**choir 进了 29 / 50 首的前三。
- **CLAP 的风格偏向 anime soundtrack。**14 / 50 首的第一名是它。这个词和用户曲库重合度高，
  可能真准，也可能只是提示词偏置，要看用户判。
- **Essentia 的风格体系是西方唱片分类。**动画歌多落到 `K-pop`、`Ballad`、`J-pop`，澤野弘之的
  配乐落到 `Gothic Metal` / `Darkwave`；钢琴类（#1、#2、#5、#25、#26）稳定落到 `Neo-Classical` /
  `Contemporary`，鼓的概率都在 0.08 以下，这一类看起来是准的。

## 3. 用户评分与结论

**待用户评分。**末三栏填完之后，按下面的规则出结论，结论和数字再补进这一节：

- 各维度的准确率：E 对的比例 =（E + 两）/ 50，C 同理。
- 建议的门槛（由用户拍板）：某一路风格准确率 ≥ 80%、乐器 ≥ 70%，就接 worker 写回 `tags`；
  风格、乐器都够不上就换路（例如按艺人打标、手动标签为主）。乐器数量单独看，预期最不准，不够
  门槛就不出这个维度，不拖累前两个上线。
