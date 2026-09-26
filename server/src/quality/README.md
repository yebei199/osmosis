# server/src/quality

音质的各音源映射,一个音源一个文件(#147,`docs/adr/0034`)。

- `netease.rs`:网易云经 bang-dream 的 `QualityLevel` 过桥,通用档位 ↔ 它的档位,
  以及一次取到的源实际是什么音质。

通用的「档位」与「实际音质」在上一级的 `quality.rs`。不负责向音源要源(那在
`routes/play`),也不负责存(`routes/play/archive.rs`)。接新音源就在这里加一个文件。
