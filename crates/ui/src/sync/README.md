# sync

同播与遥控的界面状态:设备名册、推流与收听(`syncplay`),以及遥控器那一侧
(`remote`)。

两者共用同一条信令连接(`docs/adr/0030`),所以放在一起;协议与传输在
`crates/syncplay`,这里只有界面这一层。

整组只在原生 target 上编:wasm 没有 WebRTC 之外的音频栈可推。
