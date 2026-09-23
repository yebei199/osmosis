# apps/android/src

安卓入口 cdylib 的 Rust 侧:`lib.rs` 的 `android_main` 装日志、渲染后端,把控制权交给
`ui`。其余每个模块是一座通往 Java 那一半(`../gradle/app/src/main/java/`)的 JNI 桥,
只转发、不记状态:

- `controls`:系统媒体控件(`docs/adr/0020`)。
- `downloads`:下载落进公共「音乐」目录(MediaStore)。
- `updater`:把核对过的 APK 交给系统安装器(#129)。

不管界面与领域逻辑(归 `ui` 及其下游),不管出包(归 `xtask/src/android.rs`)。
