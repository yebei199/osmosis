# audio/src/loader/tests

`loader` 模块测试里拆出去的几组，多数自己起一个本地 HTTP 服务，走真实的取流路径:

- `reconnect.rs`:断流、装死、重连只要还缺的那一段、放弃。
- `stall.rs`:流中途停摆时，解码缓冲能不能垫住声卡回调。
- `timing.rs`:开流的分段计时(`crate::open_timing`),冷连接与复用连接分得开。

不负责解码格式本身的测试(在上一层 `../tests.rs`),也不碰真实 CDN:服务都绑 `127.0.0.1:0`。
