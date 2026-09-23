Android 平台入口(cdylib)+ `gradle/` 打包工程。出包走 `cargo xtask android`
(`just android-build` / `just mcp-android`),逻辑在 `xtask/src/android.rs`。

## 签名:APK 由构建机的 debug keystore 签

`xtask` 只跑 `assembleDebug`,`gradle/app/build.gradle` 里没有 `signingConfig`,
所以**不论 native 库编的是哪一档,APK 都由构建机上 AGP 自动生成的
`~/.android/debug.keystore` 签**。`just android-build` 出的「release」包也是它,
历次发布的 APK 都是 pc1 那一把签的。

后果:换一台机器构建(那台的 debug keystore 不是同一把),覆盖安装报
`INSTALL_FAILED_UPDATE_INCOMPATIBLE`,只能卸载重装 —— 应用数据连同登录态一起清掉
(#127)。显式、可迁移的签名 keystore 由 #128 落地;在那之前,发给用户设备的包只在
pc1 上出。

同一把 key 覆盖安装不丢数据:会话、设置、设备 id 都在应用私有目录里(入口把
`internal_data_path` 交给 `ui::set_state_dir`)。连本机后端的构建与连生产的构建
在那里各用一个子目录(`osmosis-dev/` 与 `osmosis/`),同一台机上来回换装也不会
互删登录态。
