# 每个端一个平台入口 crate

`apps/` 下每个端一个 crate:`desktop`(bin)、`android`(cdylib)、`ios`(staticlib)、
`web`(cdylib + wasm-bindgen)。每个只有几十行:初始化日志、初始化渲染后端、
把控制权交给 `crates/ui`。

## 为什么

看起来更简单的做法是保留单个 crate,用 feature 切换后端 —— 这正是重构前的形态。
它撑不到六个端,原因是编译器层面的,不是审美层面的:

- **`crate-type` 是 per-crate 的,不能按 feature 切换。** Android 要 `cdylib`,
  iOS 要 `staticlib`,桌面要 `bin`。单 crate 只能取三者并集,于是每次桌面构建都在
  白造一个用不上的 `.so`。重构前的 `crate-type = ["cdylib", "rlib"]` 已经是这个
  症状的早期形态。
- **平台专属依赖会进入所有端的依赖图。** `wasm-bindgen` 不该出现在 Android 的
  依赖解析里。`[target.'cfg(...)'.dependencies]` 能缓解,但 feature 一旦统一就会失效。

`crates/ui` 唯一,因为 `slint::include_modules!()` 生成的类型只存在于编译 `.slint`
的那一个 crate 里 —— 这是 Slint 的构建模型强加的,不是我们的选择。

## 连带约束

- **crate 不能命名为 `core`。** `core` 在 Rust 的 extern prelude 里,本地包同名会让
  `use core::...` 变成歧义。客户端领域 crate 因此叫 `app-core`。`std`、`alloc`、
  `test` 同理。
- **`default-members` 只含桌面链路**(`crates/*` + `apps/desktop`)。裸 `cargo build`
  会尝试构建全部成员,而 `apps/android` 的 `android_main` 在 host target 上编不过。
  android / web / ios / server 一律靠 `-p` 显式构建。少了这一条,IDE 的
  `cargo check` 会直接全红。
