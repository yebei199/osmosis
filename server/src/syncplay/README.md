# syncplay

设备间的信令:WebSocket 的接入与消息转发(`signaling`)、设备名册(`roster`)、
校时(`clock`)。目录名是同播留下的,同播(WebRTC 推流)已删(#137);遥控器的
控制权槽位(`control`)被组的全局状态取代,已删(#142)。

组的全局播放状态(`group`,#142)也住在这里:每个账号至多一个组,状态落库
(`store::group`),意图走 HTTP(二进制里的 `routes::group`),状态经名册广播。
它是这里唯一碰数据库的部分;名册与校时仍全在内存里,进程重启即散。

客户端那一侧在 `crates/syncplay`,两边的报文形状由 `crates/contract` 钉住。
