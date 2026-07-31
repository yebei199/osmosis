# Slint 与 AI / MCP 协作开发

来源：https://slint.dev/blog/slint-and-AI-MCP

## 主题与背景

文章讨论 Slint 为何适合 "vibe coding"（AI 辅助的快速原型迭代）：AI 大幅降低了试错成本，让 UI 创意迭代几乎"零成本"，而 Slint 的架构（尤其是新引入的 MCP 支持）进一步补齐了 AI 助手与运行中界面之间的反馈闭环。

## 核心概念：Slint 的 MCP

MCP（Model Context Protocol）server **不是外部服务，而是编译进应用本身的能力**，让 AI 助手在运行时能"看到"应用内部状态，而不只是依赖截图/图像识别。具体能力：

- 实时读取布局树（layout tree），而非事后解析图片
- 检查窗口属性、元素树、可访问性（accessibility）信息
- 模拟点击、拖拽、键盘事件，驱动 UI 进行验证

## 配套能力

- **LSP 实时反馈**：语法检查、绑定循环（binding loop）检测、代码补全提示
- **热重载**：Rust 应用可用解释器模式运行 `.slint` UI，改动即时生效，无需完整重新编译
- **无头模式**：`SLINT_BACKEND=headless`，适合 CI / 容器环境；MCP 仍可截图供 AI 验证
- **多语言受益**：C++、Python、Node.js 绑定均可使用 MCP 集成

## 使用方式

官方提供 "AI Coding Assistants" 专题文档，包含 Claude Code、Codex、Cursor 的接入指南，并可配合 Figma MCP 联动设计稿。

## 技术亮点

形成 "AI → MCP → 运行中 UI → 反馈" 的闭环：AI 助手直接读取真实布局/可访问性状态并模拟交互，持续验证、发现问题并修复，而不是仅凭静态代码或截图猜测渲染结果。
