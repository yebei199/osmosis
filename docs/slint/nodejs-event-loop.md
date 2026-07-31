# Slint 与 Node.js 事件循环集成 摘要

来源：https://slint.dev/blog/slint-and-the-nodejs-event-loop

## 问题背景

Slint 的 Node.js 绑定过去存在性能问题：无论有没有实际活动，UI 线程都会每 16ms 被唤醒一次，导致空闲时持续消耗 CPU/电量，同时也让 UI 事件最多延迟 16ms 才能被处理。Slint 1.17 在 Linux 与 macOS 上解决了这个问题。

## 背景知识：事件循环

事件循环是"位于程序整个生命周期调用栈底部的无限循环"，负责等待事件、分发处理、然后重复。底层依赖平台原语：Linux 用 epoll，BSD/macOS 用 kqueue，Windows 用 I/O completion ports。

## 核心矛盾：同一线程上的两个事件循环

Slint 应用运行在 Node.js 上时，同一线程里实际有两个独立的事件循环：

- **libuv**：驱动 Node.js 的定时器和 I/O（网络、文件、DNS）
- **winit**：Slint 的窗口后端，处理平台窗口系统事件

两者必须共用一个线程，因为 Slint 的属性会在渲染时、以及从会调用 JavaScript 的 GUI 事件回调中被访问，而这些都必须运行在 Node 的主线程上；macOS 还额外要求 GUI 操作只能在主线程执行。

## 旧方案：16ms 轮询

之前的做法很简单粗暴：用 `setInterval(16)` 调用 Rust，跑一次非阻塞的 Slint 事件循环迭代后返回，这样 libuv 就能在两次 tick 之间执行。缺点很明显：空转浪费 CPU、阻止进程休眠，且给所有 JavaScript 定时器都带来最多 16ms 的延迟。

## 新方案：libuv 的 prepare hook

Slint 1.17 在 Linux/macOS 上用 [`uv_prepare_t`](https://docs.libuv.org/en/v1.x/prepare.html) hook 替换了轮询机制，实现了与 libuv 的真正集成：

prepare 回调在 libuv 每轮迭代中，JavaScript 定时器执行之后、I/O 轮询之前的最佳时机执行：

1. 通过 `uv_backend_timeout()` 获取安全的休眠时长
2. 用该超时时间运行 Slint 事件循环，阻塞等待 UI 事件或直到超时
3. 通过 `uv_async_send` 通知 libuv 立即从 I/O 轮询中退出，而不是继续阻塞

反方向（libuv 的 I/O 就绪）则通过 async-io 的 reactor 监听 libuv 的后端文件描述符来处理，一旦 I/O 可用就唤醒 Slint 的事件循环。

实现位于 Slint 仓库的 `api/node/rust/uv_event_loop.rs`，不涉及任何面向用户的 API 变化。

## 效果

Linux/macOS 上现在可以做到：
- UI 输入零延迟分发
- 空闲时进程可以真正休眠
- 空闲时 CPU 占用降为 0

## 平台现状

- **Windows**：仍停留在 16ms 轮询方案，因为 I/O completion ports 缺少公开的 libuv API 支持这种集成；Electron 用的补丁未合并进上游，且 libuv 2.x 何时被 Node.js 采用还未知。
- **Deno / Bun**：都不使用 libuv（Deno 用 tokio，Bun 用自己的运行时），因此都没有 prepare hook 可用。计划中的方案是反转所有权，让 Slint 的事件循环作为主循环，运行时通过 `slint::spawn_local` 调度 future。目前两者都是通过运行时符号解析回退到 16ms 轮询方案。
