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

不验证 Android 原生集成、物理扬声器或全部声卡驱动。当前提交是 #175 的审前测试材料，
尚未执行；CI 接入提案及环境待验证项见本树 `.dispatch/175-tests-1.md` 与
`.dispatch/175-tests-t2.md`。

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
纵向用例，默认仍执行完整集合，acceptance/run.toml 与 CI 不使用局部模式。

默认 `REMOTE_BEHAVIOR_JOBS=1`、`CARGO_BUILD_JOBS=2` 限制软件 GPU 与编译资源，执行者
获得更高额度后可以明确增加 worker；每个 testcase 独占资源，禁止自动重试和 worker 重启。
私有 DB 只监听独占 Unix socket；业务服务、媒体和信令闸由内核分配空闲端口。
固定 3000、8091 和单实例抽象 socket 仅存在于各客户端独占的 net namespace。
每台另有独占 mount/UTS namespace、hostname 文件、状态目录、D-Bus、PulseAudio socket
和 null sink。代码不调用 sethostname，不加载宿主声卡模块，不连接用户的音频或 session bus。

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
部署检查器先删除声明路径的旧结果，再跑完整 41 项用例，拒绝缺项、skip、失败或重试成绿。
Nix 固定部署检查器的提交和 SHA256，无仓库自写校验器。未需要 Rust 映射证据，因此没有
nextest 迁移。采集、位置与清理机制项也进入实际 pytest 集合与映射。

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
