# io.github.osmosis

安卓应用的 Java 那一半:只做 NDK 够不着的 framework 调用,由 Rust 经 JNI 调进来
(对面在 `apps/android/src/`)。不引入任何 androidx 依赖。

- `MainActivity`:NativeActivity 外壳,edge-to-edge 与通知权限回调。
- `MediaControls` / `MediaControlsService`:系统媒体控件与前台服务(`docs/adr/0020`)。
- `Downloads`:下载落进公共「音乐」目录。
- `Updater`:用 PackageInstaller 覆盖安装新版 APK,并接系统回来的安装状态(#129)。
