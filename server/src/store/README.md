# store

自家数据的家。连接池与迁移(`db`)、账号(`account`)、本地歌单(`playlist`)、
播放事件(`history`)、平台曲目的缓存(`cache`)。

只认 Postgres。不认识 HTTP —— 请求与响应的形状归 `routes/`;也不认识 gRPC ——
上游的东西归 `bangdream`。

`cache` 存的是平台的东西,但它不是第二份真相:写永远直发平台、冲突时平台赢、
整张删掉只是慢一次(`docs/adr/0018`)。删了会丢数据的东西不放在那里。
