---
name: device-debug
description: 观测并驱动 slint_study 三端运行中的界面——安卓真机(adb)、niri 桌面原生窗口(grim+ydotool)、web(playwright)。当需要装机、打开 app、截屏看界面、点按驱动交互、或确认某改动在真实运行时生效时使用。
---

# device-debug

在这台开发机上「看到」并「点动」slint_study 三端跑起来的界面。三端共享同一套 UI,
优先选**带控件树**的通道,退而求其次才用像素盲点。

## 选面原则(先看这条)

| 端 | 看(截屏) | 驱动(操作) | 语义 |
|----|----------|------------|------|
| web | playwright | playwright | **有控件树**,最优 |
| 安卓 | adb screencap | adb input | 有语义 input,坐标可控 |
| 桌面 | grim | ydotool | **像素盲点**,只能按坐标,兜底 |

能在 web/安卓验的交互就别在桌面上硬点。桌面 ydotool 看不到控件,只能先截图算坐标再点。

## ① 安卓真机(adb)

设备:小米 13,codename `fuxi`,model `2211133C`,serial `5a61be0f`。
包名 `io.github.slintstudy`,主 Activity `io.github.slintstudy/.MainActivity`。

```bash
# 构建 APK(要 bevy 3D 自转立方体面板用 android-build-3d)
just android-build
# just android-build-3d

# 装:默认直装
just android-install                       # = adb install -r dist/slint-study-debug.apk

# 打开
adb shell am start -n io.github.slintstudy/.MainActivity

# 截屏 → 拉到 scratchpad → Read 看
adb shell screencap -p /data/local/tmp/s.png
adb pull /data/local/tmp/s.png <scratchpad>/s.png
adb shell rm /data/local/tmp/s.png

# 操作
adb shell input tap X Y                     # 坐标见截图(注意截图分辨率与实际的换算)
adb shell input text "hello"
adb shell input keyevent KEYCODE_WAKEUP     # 亮屏;BACK/HOME 等同理

# 当前前台 Activity(确认 app 真在前台再截图)
adb shell dumpsys activity activities | grep topResumedActivity
```

装机命令走 `adb install` 前台跑并加 `timeout`,别后台挂着——传输一旦卡死会静默等下去。

### MIUI 安装绕法(fallback)

直装报 `INSTALL_FAILED_USER_RESTRICTED: Install canceled by user` = MIUI「通过 USB 安装」
开关没生效(它联网校验后会悄悄回退)。先让用户去开发者选项重开该开关;仍不行就绕:

```bash
adb push dist/slint-study-debug.apk /data/local/tmp/x.apk
adb shell pm install -i com.android.vending -r /data/local/tmp/x.apk   # 伪装安装来源,稳过
adb shell rm /data/local/tmp/x.apk
```

### 连接踩坑

- `adb devices` 显示 `device` 但 `adb shell echo ok` 超时 = **数据通道死、握手还活**,是 USB 线/口硬件问题。换机箱后置口、换确定能传数据的线;别用扩展坞/前置口。
- 大流量(装 57MB)卡死、小命令也卡 = 同上,换口。
- 状态 `authorizing` / `unauthorized` 反复 = 授权没勾"一律允许"且息屏看不到弹窗。**手机解锁亮屏**,弹窗勾「一律允许」再点允许;必要时开发者选项「撤销 USB 调试授权」后重插。

## ② niri 桌面原生窗口(grim + ydotool)

Wayland 会话(niri)。grim 截屏,ydotool 注入输入(ydotoold 已常驻,socket `/run/ydotoold/socket`)。

```bash
# 跑桌面版(按项目 just/cargo 方式启动 desktop app,略)

# 截屏
grim <scratchpad>/desk.png                  # 整个输出
grim -g "$(slurp)" <scratchpad>/desk.png    # slurp 框选区域(交互式,需用户拉框)
# 然后 Read <scratchpad>/desk.png

# 操作(像素盲点:先截图算出目标像素坐标)
ydotool mousemove --absolute -x X -y Y
ydotool click 0xC0                           # 左键单击(0xC0 = down+up)
ydotool type "hello"
ydotool key 28:1 28:0                         # 回车(keycode 28 down/up)
```

ydotool 用的是 uinput,看不到控件树,只认屏幕像素坐标——每次点前都得先 grim 一张、量坐标。
若 `ydotool` 报连不上 socket,用 `YDOTOOL_SOCKET=/run/ydotoold/socket ydotool …`。

socket 权限:ydotoold 跑在 `root:ydotool` 组、0660。用户 `yb` 已由 nix
(`nixos_config/hosts/reusable/ai-gui-automation.nix`)加入 `ydotool` 组。
若 `getent group ydotool` 里有 yb 却仍 `Permission denied`:补充组只在**完整 PAM 登录**时刷新,
Claude Code 进程从旧父进程继承组集——**重启终端/会话不够,得彻底注销图形会话再登录**
(登出到 display manager 或重启),再新开终端起 Claude Code。届时 `id` 里才有 ydotool。
沙箱内 `sg`/`newgrp` 临时切组会被 `setgroups: Operation not permitted` 挡掉,别走这条。

## ③ web(playwright MCP)

```bash
# 编 wasm + 起静态服务(可透传 feature,如左上角帧率:just web-dev ui/debug-fps)
just web-dev
# → http://127.0.0.1:8080 ;server-dev 另在 127.0.0.1:3000(「Check server」按钮打它)
```

浏览器侧全走 playwright MCP `browser_*` 工具,**原生带控件树**,别用 grim 截桌面浏览器:

- `browser_navigate` 开 `http://127.0.0.1:8080`
- `browser_snapshot` 拿可访问性树(定位元素、拿 ref)
- `browser_take_screenshot` 截图看渲染
- `browser_click` / `browser_type` / `browser_press_key` 驱动
- `browser_console_messages` 看前端报错

安卓要连本机 server-dev 时:`just android-reverse`(= `adb reverse tcp:3000 tcp:3000`)把手机的 3000 转发到开发机。
