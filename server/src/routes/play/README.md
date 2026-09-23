# routes/play

一首歌的音频怎么交到客户端手上。三条路共用同一条上游取源、同一个档位
(`play.rs` 的 `PLAY_QUALITY`),否则「听到的」「下载的」「存下的」会是三个版本。

- `play.rs`(上一级):`GET /play/{id}`,交出一条客户端自己去取的链接 ——
  存过的给对象存储的签名链接,没存过的给网易云的临时直链。
- `download.rs`:`GET /download/{id}`,把字节拉过来、归一成 mp3 交出去。
- `archive.rs`:听过的歌存进对象存储(#126)—— `/played` 之后后台存、
  `/play` 优先从这里交付、定时清掉没人红心且三天没播的。

不负责对象存储怎么连(`server::objects`)和账目怎么落库(`server::store::archive`),
也不负责起播上报本身(`routes/library/history.rs`,它只是在报完之后调一下这里)。
