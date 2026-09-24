# syncplay/src

设备之间的信令客户端(`docs/adr/0030`)。

- `signalling.rs`:一条 `/signal` WebSocket 连接，收发 `contract` 的信令报文;
- `client.rs`:编排重连、接管、换输出、校时与播放组事件，对外是 `Client` 与 `Event`;
- `clock.rs`:与服务端校时 —— 多次往返挑最短的，拟合偏移随时间的漂移，把服务端时刻换算成
  本机单调时钟(#137 ⑤ 播放组的共同计划都写在服务端时钟上);
- `session.rs`:同账号在线名册的过滤。
