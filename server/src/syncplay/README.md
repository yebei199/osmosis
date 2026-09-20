# syncplay

同播与遥控的信令:WebSocket 的接入与转发(`signaling`)、设备名册(`roster`)、
控制权归谁(`control`)。

与音乐那半毫无关系,state 也不共用 —— 唯一的交集是账号。也不碰数据库:
这里的东西全在内存里,进程重启即散(`docs/adr/0030`)。

客户端那一侧在 `crates/syncplay`,两边的报文形状由 `crates/contract` 钉住。
