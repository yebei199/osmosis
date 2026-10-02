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
尚未执行；CI 接入提案及环境待验证项见本树 `.dispatch/175-tests-1.md`。

审后运行入口为 `just ci-remote`，完整 `just ci` 也包含它。CI workflow 的非 draft PR
事件及 `workflow_dispatch` 执行相同 Nix shell 和 `run.sh`，不新增 push 触发。
实施者重编译仍使用派工已有的 remote-run/verify-run 空闲机器，不在 pc1 本地运行。

`env.nix` 固定 nixpkgs tarball 和内容哈希，复用 `slint.nix` 的 native 库，额外声明
pasta、PulseAudio、ALSA pulse plugin、PostgreSQL、Xvfb、lavapipe、namespace 工具和 protoc。
Python 依赖由 `uv.lock` 固定。宿主须是允许无特权 user/net/mount/UTS namespace 的
Linux，使用普通用户。缺依赖、缺 namespace 或 GPU adapter、初始化和采集失败均非零，
没有 skip 路径。CI 的 AppArmor 配置只发生在一次性 runner。

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

`acceptance.toml` 是 #239 试点草案，含逐 AC 的 id/kind/entry/observe/tests/doubles/mutation。
映射 ID 采用 `JUnit classname::name`，每个实际 testcase 身份唯一，多个 AC 可引用同一证据。
`pytest --junitxml` 产生实际结果后，`verify_junit.py` 拒绝空集合、缺项、重复、skip 或失败。
这份局部核对不定义共享治理协议。未需要 Rust 映射证据，因此没有 nextest 迁移。

运行入口打印 `artifacts=`，证据留在该独占临时目录：原始 PCM、UI 动作（填充值去除）、
媒体输入哈希、环境/候选哈希、命令日志、进程退出凭据、JUnit 与验收核对结果。
四种变异分别在独立 clone 修改一个故障点，保留 patch、编译日志及定向 RED/GREEN JUnit。
编译/启动错误不算 RED。正常退出和测试失败都会清理登记资源，日志和 PCM 留供复核。
强制 SIGKILL 或宿主失联无法保证执行 finally，必须以资源凭据判未完成，不声称已清理。
