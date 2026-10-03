# apps/desktop/src

桌面平台入口(二进制 `osmosis-desktop`):初始化日志、占启动锁、配渲染后端，然后把控制权交给 `ui`。
界面、播放、同播都不在这里，这里只放桌面独有的胶水。

- `main.rs`:入口，按上面的顺序串起来。
- `single_instance.rs`:启动锁，同一档同一时刻只许一个实例(#135)。
- `mpris.rs`、`mpris/`:linux 的 MPRIS,媒体键与系统播放控件。
- `log_file.rs`:日志落盘(#145),见下。

## 日志文件

装机版从启动器起,stdout/stderr 接的是 `/dev/null`,所以日志同时写进状态目录里的一个文件:

- 装机版:`~/.local/state/osmosis/osmosis.log`
- 开发实例(没烘生产地址的构建):`~/.local/state/osmosis-dev/osmosis.log`

设了 `XDG_STATE_HOME` 的话，把 `~/.local/state` 换成它。单个文件满 5MB 就滚：旧的依次改名成
`osmosis.log.1`、`osmosis.log.2`,只留这两个，总量不超过 15MB。启动时接着往后写，不清空，
所以重启之后上一次运行出事的那段还在。每次启动的第一行是版本号与 pid。

级别与 stderr 相同：默认 `info`,设了 `RUST_LOG` 就照它来。stderr 照常输出。

不依赖任何平台之外的东西：安卓走 logcat,不用这个文件。

## 公共音乐下载

桌面下载由这里实现 `ui::DownloadStore` 并在入口注入。音乐目录集中解析为 XDG 的 Music
目录，取不到时使用 `$HOME/Music`，歌曲存在其中的 `osmosis/`。字节先写入隐藏临时文件，
收全后原子发布；同名成品保留，新件追加递增编号。断流与丢弃会清理待定文件。
入口初始化仅回收 `.osmosis-download-*.pending` 普通文件，保留其他文件、目录与软链。
关窗的 `_exit` 架构未改，残留待定文件在下次启动时清理，属于缓解。
