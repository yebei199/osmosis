# assets

派生给各端的那份图标源,以及桌面的 `.desktop` 文件。

| 文件 | 是什么 |
|------|--------|
| `io.github.osmosis.svg` | 图标源。24×24 网格,竖线当半透膜、左密小点右疏大点 |
| `io.github.osmosis.desktop` | 桌面项。`Icon=` 与上面那份的文件名 stem 对齐 |

## 派生关系

**只有这一份源,没有导出的 png。** 两端都吃矢量:

- 桌面 —— `io.github.osmosis.svg` 原样装到
  `~/.local/share/icons/hicolor/**scalable**/apps/`(`just desktop-install`)
- 安卓 —— `apps/android/gradle/app/src/main/res/` 下的
  `drawable/ic_launcher_foreground.xml` 与 `ic_launcher_background.xml`,
  由 `mipmap-anydpi-v26/ic_launcher.xml` 组成自适应图标。minSdk 是 26,
  `anydpi-v26` 对任意密度都命中,所以那一堆 `mipmap-hdpi/*.png` 一个都不需要
- 通知栏小图标 —— `drawable/ic_media_notification.xml`,同一批形状抹掉颜色,
  系统只看 alpha

前景层是把 24 网格 ×3 再平移 18 摆进 108 画布的中央安全区。**改形状要四处
一起改**:本目录这份 svg、安卓那两份 vector、通知那份。手算圆弧容易错,
`M{cx-r},{cy}a{r},{r} 0 1,0 {2r},0a{r},{r} 0 1,0 {-2r},0Z` 是那个套路。

## 装了才生效的那一样

MPRIS 的 `DesktopEntry` 属性报的是 `io.github.osmosis` 这个 id
(`apps/desktop/src/mpris.rs`)。外壳拿它回头去 XDG 的 applications 目录里找,
`.desktop` 没装进去的话查不到,bar 上就只剩 `Identity` 那行文字、旁边没有图标。
`just desktop-install` 是把它装进去的那条路。
