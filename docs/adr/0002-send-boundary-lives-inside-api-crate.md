# `Send` 边界关在 `api` crate 内部,`app-core` 的 future 不要求 `Send`

应用是异步的,且在 native 上用多线程 tokio runtime 跑 IO。但 `crates/app-core` 里的
`async fn` **不要求 `Send`**,它们由 UI 线程上的 `spawn_local` 驱动。需要 `Send` 的
只有 `crates/api` 内部丢给后台 runtime 的那些 future。

## 为什么

三件事经常被混为一谈,它们其实互相独立:

- **异步**(`async fn`):wasm 完全支持。
- **多线程**:wasm 不支持。
- **`Send`**:它不是"异步"带来的,是 `tokio::spawn`(多线程 runtime)带来的。

如果让 `app-core` 的 future 直接被 `tokio::spawn`,`Send` 就会成为 `app-core` 每个
`async fn` 的隐含约束。而 wasm 上 `fetch` 返回的 future **不是 `Send`**。于是
`app-core` 要么另写一份 wasm 实现,要么撒满 `#[cfg(target_arch = "wasm32")]`。
前者意味着领域逻辑维护两遍并悄悄漂移,后者意味着领域逻辑被平台细节淹没。

把 `Send` 关在 `api` 内部后,`api` 对外暴露的 `async fn` 在两端**签名完全相同**:

```rust
pub async fn health() -> Result<HealthDto, ApiError>;
```

native 实现内部把 `reqwest` 的 future(它是 `Send`)交给后台 runtime;
wasm 实现内部直接 `await` `fetch`。差异到 `api` 为止,不再向上传播。
`app-core` 一份代码通吃,可纯单元测试,无 `cfg`。

## 代价

`app-core` 自身的逻辑始终跑在 UI 线程上。如果将来出现重 CPU 的领域计算,必须**显式**
把它丢给线程池(native)或 web worker(web),而不能指望多线程 runtime 自动帮忙。
这是一个逐调用点的决定,不是一个全局属性 —— 我们认为这样更好。

## 连带约束

从后台线程回到 UI 线程走 `slint::invoke_from_event_loop(f)`,要求 `f: FnOnce() + Send`。
因此 `contract` 与 `api` 中跨越该边界的类型必须是 `Send`。
**不要在这些类型里放 `Rc`** —— 一旦放了,这条路当场断掉。
