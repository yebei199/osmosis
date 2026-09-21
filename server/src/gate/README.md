# gate

请求进门那一道:请求头里的 token 换成账号(`auth`),以及按账号计的限流(`ratelimit`)。

不负责任何业务判断 —— 这里只答「这条请求能不能往下走」。也不负责账号本身的规则,
那在 `store::account`,可以脱离 axum 单独测。

两边都有人用:HTTP 走 `Account` 提取器(为什么是提取器而不是中间件,见 `server/README.md`),
信令的握手在升级之前也要过同一道鉴权。

限流不再是各个 handler 自己调的一句话,而是**挂在路由组上的一层**
(`main.rs` 里那几个 `*_routes`):六条策略各一个桶,键取 extensions 里那份
已认证的账号 id —— 不是 token,同账号多 token 会让总额度随 token 数放大。
它的状态在进程内存里,所以是**每实例削峰**;跨副本严格成立的只有数据库那道
队列数上限(`store::queue::MAX_QUEUES_PER_ACCOUNT`)。
