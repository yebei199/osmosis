# 0020. 系统媒体控件的后端住在平台入口

日期:2026-08-06
状态:已接受。是 0003「一端一个入口 crate」在能力层上的一次具体应用。

## 背景

桌面外壳(DMS/quickshell、waybar、GNOME 的锁屏控件)认播放器只有一条路:session bus
上叫 `org.mpris.MediaPlayer2.*` 的名字。安卓那边是 `MediaSession` 加一条挂在前台服务上
的通知。两者要的东西是同一样 —— 此刻在放什么,以及外面按下的键该落到哪 —— 但说法
完全不同:一个是 D-Bus 接口,一个是 Java 对象。

在此之前 osmosis 两样都没有。声音走 rodio → cpal → ALSA/PipeWire,那条路上没有歌名,
于是外面没有任何办法知道这里在出声。

## 决定

`crates/ui` 只放接缝与调用点:`NowPlaying`、`MediaCommand`、`MediaHooks`、
`trait MediaControls`。**两个后端都住平台入口 crate** —— zbus 那份在 `apps/desktop`,
JNI 那份在 `apps/android`。入口把实现交给 `ui::run_with_renderers` 的第三个参数,
形状是 `FnOnce(MediaHooks) -> Box<dyn MediaControls>`。

不新建能力层 crate,也不像 `audio` / `syncplay` 那样由 `ui` 直接持有。

## 理由

- **安卓那份别无选择。** `MediaSession`、`Service`、`Notification` 都是 Java 框架类,
  Rust 侧要 JavaVM 与 Activity 句柄,而它们只在 `android_main(app: slint::android::AndroidApp)`
  里存在。`crates/ui` 不依赖 `android-activity`,也不该依赖 —— 它够不着那个句柄。
- **入口本就是组合根。** 这条规矩 `apps/android/src/lib.rs` 里已经写着:「入口 crate
  本就同时依赖 ui 与 render3d,是接 seam 的天然组合根」。媒体控件与 render3d 是同一种
  东西 —— 平台特有、由入口注入、ui 只认接缝。
- **界面层不该认识 D-Bus。** Linux 那份技术上可以就地放进 `crates/ui`(zbus 是纯 Rust),
  但那会让界面层多出一条只有一端用得上的协议依赖。它连 tokio 都刻意不认识
  (0002),没有理由为 D-Bus 破例。两个后端住同一类位置,下一个人也就只有一个地方要找。
- **不值得为一个 trait 开一个 crate。** 能力层 crate 要连带改 workspace manifest、
  `xtask boundaries` 的禁用表、justfile 与 CI 各两条 `-p`,而里面只有一个 trait 和
  两个平台各自的实现 —— 那两份实现按上一条本来就该住入口。

## 接缝的形状

**单位统一毫秒,换算全在 ui 侧。** `Play`/`Pause` 与 `Toggle` 的区分、相对跳转换成
绝对位置、绝对位置换成 Slint `seek` 要的比例,都在 `crates/ui/src/media.rs` 做完。
后端因此不必记住任何状态。让两个后端各记一份「现在是不是在放」,迟早有一份是错的。

**封面给两份。** `art_url` 是平台给的 CDN 链接,MPRIS 的 `mpris:artUrl` 直接用;
`art` 是 `crate::cover` 解出来喂点云的那份 ≤512px RGBA,安卓拿它转 `Bitmap` —— 那边的
`MediaMetadata` 不接受 http URL,通知栏不会替你去下图。两份 ui 本来都攥在手里,
谁都不必再下一次。反过来给 D-Bus 塞 512×512 的裸像素才是错的。

**命令只调 `.slint` 的回调。** `bind_controls` 里已经有一整套规矩(收听同播时先退出、
放空了就重放当前曲),媒体控件那条路重写一遍就会立刻长歪。这也顺带守住了投影的
单一写者(见 `crates/ui/src/notice.rs` 的那条测试)。

## 代价

- **`run_with_renderers` 的签名变了。** 两个入口各改一处。`run()` 那条路(web / iOS)
  不带媒体控件,传 `NoControls`。
- **安卓侧没有自动化测试。** gradle 里零 `androidTest`/`testImplementation`,本次也不铺:
  那会破掉「plain Android framework APIs only」的现状,而且 CI 里没有模拟器跑不了。
  回归靠这几条可复现的命令加真机肉眼:

      adb shell dumpsys media_session
      adb shell dumpsys activity services io.github.osmosis
      adb shell input keyevent 85

  这是个真缺口,写在这里而不是假装它不存在。Linux 侧不同 —— 那边有一条自己 spawn
  `dbus-daemon` 的真总线集成测。
- **`CoverUpdate::Show` 改成了 `Arc`。** 同一张图现在有两个去处(点云与媒体控件),
  兆级的字节不值得拷两份。`apps/*` 的 seam 代码一字未动 —— `Arc` 自动解引用。
