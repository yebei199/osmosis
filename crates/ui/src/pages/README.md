# pages

整页的绑定:登录页(`account`)、个人主页(`profile`)、搜索页(`search`)。

「我的库」那两块归 `../library`,音乐页归 `../music` —— 那是最大的一页,
自己一个目录。

会话失效由 `account::handle_session_expiry` 统一处理:任何一条路由拿到
`unauthorized` 都走它,不各写各的。
