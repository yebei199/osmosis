# tests

`ui` crate 的无头集成测试:用 `i-slint-backend-testing` 建出真实的 `MainWindow`,
不起窗口系统,靠 `ElementHandle` 读元素树、点元素、量属性。钉的是**界面这一层的
行为**——组件在不在、状态跟数据变没变、几何有没有钻到别的东西底下——不是渲染像素,
也不是 `app-core`/`api` 那些领域逻辑的正确性(那些各有自己 crate 里的单测)。

一个文件对应一块界面行为(`liked.rs` 红心、`thumbnail.rs` 封面槽位、
`queue_page.rs` 队列覆层……),不是一个 `.slint` 文件对一个测试文件。

## 不负责

- 像素级外观(颜色、描边、间距实测)——测试框架不导出通用属性读取,只有
  `accessible_*`、`size`、`absolute_position`、`computed_opacity` 这几个公开口子,
  断言只能落在这些上面,或落在能间接观察到的数据/几何变化上。
- 真机专属的行为(手势条、真实触摸)——那要 `just mcp-android` 配真机跑,
  见仓库根 `AGENTS.md`。
