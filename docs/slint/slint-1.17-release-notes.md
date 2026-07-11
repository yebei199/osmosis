# Slint 1.17 发布说明摘要

来源：https://slint.dev/blog/slint-1.17-released
发布日期：2026-06-24

## 主要新特性

### 拖放交互（Drag and Drop）
- 新增 `DragArea` / `DropArea` 组件，支持应用内拖放交互。
- 相关属性/回调：`allow-copy`、`can-drop`、`dropped`、`drag-finished`、`contains-drag`、`proposed-action`、`make-transfer`、`read-transfer`，以及 `data-transfer`、`DragAction` 类型。
- 跨应用拖放（drag between applications）仍在上游开发中，未随本版本发布。

### 系统托盘图标
- 新增 `SystemTrayIcon` 组件，支持 macOS / Windows / Linux，可自定义菜单和快速操作项。

### Tooltip 与 RadioGroup
- 新增 `Tooltip` 元素，可附加到任意界面元素上显示提示文本。
- 新增 `RadioGroup` 组件，自动管理一组 `RadioButton` 的选中状态和键盘导航。

## 其他改进

- **AI / MCP 集成**：支持 Model Context Protocol (MCP) server，AI 助手可通过可访问性树（accessibility tree）检查运行中的应用。
- **远程查看器**：`slint-viewer` 支持 `--remote` 标志，连接远程 LSP 服务查看/调试。
- **移动端**：Android、iOS 应用已正式发布；支持从 C++ 直接构建 Android 应用。
- **数据绑定**：模型（Model）的行支持双向绑定（two-way bindings on model rows），可直接编辑模型数据。
- **布局**：`HorizontalLayout` / `VerticalLayout` 新增 `cross-axis-alignment` 属性。
- **文本**：`StyledText` 支持运行时解析 Markdown。
- **阴影效果**（Skia 后端）：`Rectangle` 新增 `drop-shadow-spread` 及内阴影（inner-shadow-*）相关属性。
- **性能**：Node.js 集成在 Linux/macOS 上的空闲 CPU 占用降至接近 0。

## Breaking Changes

文章未提及任何 breaking changes 或 API 废弃/迁移说明。

## 相关链接

- 官方文档：https://docs.slint.dev
- GitHub Release / ChangeLog（详见博客原文链接）
- Mattermost 社区
- "Made with Slint" 项目展示页
