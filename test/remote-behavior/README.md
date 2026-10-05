# 遥控行为验收

这里负责从真实桌面控件到另一客户端音频输出的 Linux 软件链路验收。媒体服务只替换
外部音乐账户与目录，数据库、业务路由、信令、播放器和解码器使用生产实现。独立网络
namespace 承接桌面单实例锁与编译期 API 地址；每个客户端使用自己的状态目录和虚拟音频服务。

用例、资源生命周期和运行入口共同维护，入口是 `run.sh`，pytest 集合由 `test_remote.py`
定义，`cases.py` 实现用户路径。`ui.py` 沿用既有 MCP；`resources.py` 登记与回收本轮资源；
`media.py` 只供应外部目录和确定素材，同时独立识别 monitor PCM；`bridge.py` 在目标
namespace 请求该客户端的 MCP。
测试失败保留资源日志与 PCM，临时进程和数据库仍精确清理。`mutate.py` 只改临时固定快照，
保留每一种独立变异的 patch 和行为失败结果。

不验证 Android 原生集成、物理扬声器或全部声卡驱动。#175 测试设计已经审过，正在执行
真实链路验证；运行结果与未决项以本树 `.dispatch/175-handoff-*` 的固定候选报告为准，
不能将设计通过视作功能通过。设计见 `.dispatch/175-tests-t2.md`。

审后运行入口为 `just ci-remote`，完整 `just ci` 也包含它。CI workflow 的非 draft PR
事件及 `workflow_dispatch` 执行相同 Nix shell 和部署检查器，检查器消费
`acceptance/run.toml` 后调用 `run.sh`，不新增 push 触发。
实施者重编译仍使用派工已有的 remote-run/verify-run 空闲机器，不在 pc1 本地运行。

`env.nix` 固定 nixpkgs tarball 和内容哈希，复用 `slint.nix` 的 native 库，额外声明
pasta、PulseAudio、ALSA pulse plugin、PostgreSQL、Xvfb、lavapipe、namespace 工具和 protoc。
Python 依赖由 `uv.lock` 固定。宿主须是允许无特权 user/net/mount/UTS namespace 的
Linux，使用普通用户。缺依赖、缺 namespace 或 GPU adapter、初始化和采集失败均非零，
没有 skip 路径。CI 的 AppArmor 配置只发生在一次性 runner。
uv 显式选择 Nix Python 并禁用 managed Python，grpc wheel 的 libstdc++ 由 Nix 声明。
资源启动前实际核验 pidfd 与依赖导入；缺能力直接失败。`run.sh list` 仅选首条真实列表
纵向用例，`run.sh radio` 仅选旧电台引用保护用例；默认仍执行完整集合，
acceptance/run.toml 与 CI 不使用局部模式。

证据目录默认落在 `~/.cache/osmosis-remote-behavior/175-rb.*`。`run.sh` 退出时（含失败与中断）
由 `retention.sh` 只保留最近 `REMOTE_BEHAVIOR_KEEP_RUNS`（默认 3）个运行：成功的删掉
`build/` 与 `fault-*/{build,snapshot}`，失败的整目录保留，更早的整个删除。没有 `exit.txt` 且
一天内改动过的目录视作正在运行，不删也不占名额；root 为空或 `/` 时拒绝执行。

`run.sh targeted test_remote.py::<用例名>...` 只选择点名的 Python 用例，用于开发阶段
的局部验证；缺少用例名或没有收集到测试都会失败。默认 `all` 的累计集合保持不变，
验收映射门禁仍执行全部映射，完整验收按交付候选的统一检查计划执行。

电台引用保护先向目标转交并暂停，保存原 queue/revision/entry。源端离组经有界确认后
打开电台，再明确点击一次处于暂停态的真实播放按钮；导航本身不保证继续。PCM 确认
源端本机播放，同时断言组仍暂停且原引用未变。续取只接受动作后服务端时间戳的新
playing 上报，revision 取该上报实际应用的版本；至少四个新版本须属于原队列。
`local-fm-premise.json`、`local-fm-publications.jsonl` 和 `transport-controls.jsonl`
保留该前提；最终目标恢复旧曲、旧 entry 存在、追加/替换与静音判据保持不变。

播放入口回归新增四个真实 Python testcase：FM/列表保留本地曲目离组后的可点击入口、
真正空本机队列、组内非出声成员的全局投影。入口子树在至少六秒有效静音采集前后
各核对一次，覆盖生产轮询刷新；音频执行用实际 PCM，新 checkpoint 只作辅助凭据。
原始播放条树保存于 `playback-entry-trees.jsonl`，不把预期曲目写成 UI 状态。

`nextest.toml` 选择 `music::tests::group` 的 Rust 回归，包括本地播放入口、组开关投影、
电台 seed 接管及拒绝迟到应答；夹具无声卡，调用真实状态机与投影函数，
输入 Idle 与实际队列，不人为设置 `has_track`。
它们只补快速层，不抵扣 Python 音频。完整 `run.sh` 生成独立 nextest JUnit 和
pytest JUnit，分别复制为 `results/projection-junit.xml` 与 `results/junit.xml`；
`acceptance/run.toml` 声明两者，失败、缺结果或空选择均非零。nextest 无重试，
依赖由 Nix 显式声明；首轮设计审前未执行这些用例，也未修改生产。

默认 `REMOTE_BEHAVIOR_JOBS=1`、`CARGO_BUILD_JOBS=2`。#184 在 12 核 pc2 上实测过并行：
场景是 CPU 密集型（真实解码加三个软件渲染客户端），2 路总耗时反比串行长，2 路与 4 路都
出现客户端音频落后挂钟的时序失败，并行收益被争用抵消，所以仍串行。每个 testcase 独占
资源，JUnit 的 `artifacts` 属性指回它的资源目录；禁止自动重试和 worker 重启。
私有 DB 只监听独占 Unix socket；业务服务、媒体和信令闸由内核分配空闲端口。
固定 3000、8091 和单实例抽象 socket 仅存在于各客户端独占的 net namespace。
每台另有独占 mount/UTS namespace、hostname 文件、状态目录、D-Bus、PulseAudio socket
和 null sink。代码不调用 sethostname，不加载宿主声卡模块，不连接用户的音频或 session bus。
客户端由 unshare 显式创建 user/net/mount/UTS namespace，保留宿主 PID namespace；
wrapper 报告的 PID 必须等于实际持有的命令 PID，才用于音频绑定和停止。pasta 仅接入
已核对的本轮 namespace，不自行创建 PID namespace。lavapipe 通过现有 SLINT_WGPU_CPU
开关允许软件 adapter，没有修改渲染生产逻辑。
登录在真实 LoginPage 内要求唯一可见的登录 HoverButton，并点击该按钮唯一可见的 touch。
标签属于按钮根，touch 承接实际点击。每个客户端保留填值前的原始 MCP 子树与提交按钮
根/touch 的属性、句柄和唯一性记录，账户填值不进入证据。

启动前三台各采有效静音窗口。每次音频采样保留原始 S16LE、字节偏移、采样时间和
音频图：应用 PID/birth → sink-input → 私有 sink → monitor → 本轮 parec PID。
夹具只提供 HTTP WAV，不连接音频服务。左声道频率标记曲目，右声道频率标记媒体秒数；
已知音频与静音都要求至少一秒有效采集，采集不足不能当静音。暂停、继续和双向 seek
从 PCM 判定；数据库只补充核对实际采到的位置与组时间线。

接续操作先建立至少十二秒、推进至少两秒的真实 PCM；操作前后的原始窗口带单调时钟。
`position.py` 从旧末帧的一秒区间与实际经过时间推导范围，沿用 1.5 秒容差且只加一次。
暂停生效夹在请求发出和首个有效静音窗口之间；保持至少六秒连续有效静音后，继续生效
夹在请求发出和新首帧之间。操作后 DB 不参与独立边界的推导，断言失败不重试。
`test_position.py` 的构造反例仅验证判据，不能抵扣真实路径与 PCM 证据。

根 `acceptance/175.toml` 使用正式格式 `[[ac]]` 与 doubles 列表，保留全部回归项。
映射 ID 采用 `JUnit classname::name`，每个实际 testcase 身份唯一，多个 AC 可引用同一证据。
`pytest --junitxml` 的真实结果原样保留于独占 artifacts，并复制到 run.toml 声明的相对路径。
部署检查器先删除声明路径的旧结果，再运行完整映射集合，拒绝缺项、skip、失败或重试成绿。
Nix 固定部署检查器的提交和 SHA256，无仓库自写校验器。nextest 仅增加本次投影证据，
没有迁移无关 Rust 套件。采集、位置与清理机制项也进入实际 pytest 集合与映射。

运行入口打印 `artifacts=`，证据留在该独占临时目录：原始 PCM、UI 动作（填充值去除）、
媒体输入哈希、环境/候选哈希、命令日志、进程退出凭据、JUnit 与验收核对结果。
四种变异分别在独立 clone 修改一个故障点，保留 patch、编译日志及定向 RED/GREEN JUnit。
证据目录建立于持久 XDG cache 下的 osmosis-remote-behavior，或明确指定的
REMOTE_BEHAVIOR_ARTIFACT_ROOT；每次独占，不使用退出即删除的 Nix TMPDIR。
编译/启动错误、超时和取消不算 RED。`lifecycle.py` 通过独占 IPC 持有 guardian，
`guardian.py` 使用 Linux 子收割器与 pidfd 持有后代出生身份，跨 session 的后代也受托管。
只遍历本树实际父子关系，不按名称或全机扫描；leader 先退后仍持有后代。先 TERM，
有界等待后冻结持有树、KILL 并收割，实际 children 为空才写 complete；否则非零失败。
父端断开 IPC 也触发回收。`lifecycle.sh` 是 run.sh 与轻量取消测试共同使用的 shell
收口入口；外层 TERM/INT、嵌套 pytest 超时和部分初始化失败都进入此路径。
退出/超时/取消状态与清理结果分开记录，日志与 PCM 留供复核。`test_lifecycle.py` 验证机制，
不替代音频业务证明；轻量验证命令与故障场景见 t2 设计文书。
强制 SIGKILL guardian 或宿主失联无法保证收口，必须以资源凭据判未完成，不声称已清理。

断连闸保留 server 半连接以触发真实心跳清退；最后输出端用例等待上限
为 100 秒，覆盖生产 30 秒间隔、两次容忍与最坏相位，不改心跳配置。

私有 monitor 采集显式请求 100ms 延迟，避免默认约 2 秒缓冲导致采集批量到达
并干扰短缓冲输出；实际配置随音频图保留，PCM 窗口、静音及位置判据不变。

四项变异从本轮已结束的构建完整复制或 reflink 到各自 target；不硬链接可写文件。
每份独有源码仍以原 cargo 命令重编校验，复制命令、来源候选和实际 binary SHA
分别留下原始日志，RED 与恢复候选仍进入相同精确用例及最终 PCM 判据。

最后输出端断连使用独有 240 秒素材（WAV、上游时长和 PCM 时间编码一致），
覆盖心跳最坏90秒及真实重连退避最大75秒，避免90秒素材先到末尾。
继续之前要求暂停版本实际到达重连目标端，等待上限为60×1.25+10秒。
其他用例保留90秒输入；原静音、暂停、真实继续及最终PCM断言不变。

PostgreSQL、D-Bus、PulseAudio及XDG runtime socket使用本轮mkdtemp短目录，
不随变异证据目录层级增长；日志、PCM、数据与输入摘要仍独立持久保留。
所有进程停止后删除本轮runtime目录，路径及删除结果写入resources.json。
每个客户端的 HOME 也属于自己的证据目录，平台初始化与音乐下载不会访问宿主音乐目录。

#181 的 `test_dislike.py` 从真实抽屉与列表上下文菜单验证三种不喜欢理由，观察私有库
规则/反馈、当前列表和队列、重启/恢复及真实 PCM。列表 `Expand` 与长按共用生产回调，
遮罩取消通过现有导航位置的 MCP 指针点击进入。它不证明物理手指长按的计时器或 Android
原生端；小米13另排实机时段。开发入口为 `run.sh targeted test_dislike.py`，累计验收
同时运行 app-core 屏蔽单元测试并生成既有 projection JUnit。
兼容用例固定旧版三枚举，检查缺省规则响应可解析，新客户端声明 song_rules 后可管理歌曲规则。
