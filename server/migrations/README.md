# server/migrations

数据库 schema 的演进,唯一落点。`server/src/store/db.rs` 里的 `sqlx::migrate!()`
在服务端启动时按文件名顺序应用这里的每一个 `.sql`。

## 负责什么

- 建表、加列、加索引、加约束。
- 修正**数据库里**的说明文字(`COMMENT ON TABLE` / `COMMENT ON COLUMN`)。

## 不负责什么

- 查询与读写逻辑 —— 那在 `server/src/store/`。
- 面向开发者的架构说明 —— 那在 ADR(`docs/adr/`)和各处的模块文档。

## 一条硬规则:已经应用过的文件一个字节都不能改

sqlx 对每个已应用的迁移比校验和,而校验和算的是**整个文件内容,SQL 注释也算**
(`sqlx-core` 的 `migrate/migrator.rs`:不符就返回 `VersionMismatch`)。改动一个
已应用文件里的一行注释,服务端下次启动就会在 `db.rs` 那句 `?` 上直接失败。

所以:发现旧迁移里的注释与代码脱节时,**新增一个迁移**把正确说法写成
`COMMENT ON ...`,或者写进 `server/src/store/` 的模块文档,而不是回头改那个文件。
`0007_platform_tracks_comment.sql` 就是这么来的(#109 的 F-001)。

## 依赖

- `server/src/store/db.rs` —— 应用它们的地方。
- 生产库已经应用到哪一版,以 `_sqlx_migrations` 表为准。
