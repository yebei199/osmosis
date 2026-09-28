//! 屏蔽规则的线上格式(#161)。屏蔽是规则,不是评价:踩只表态,
//! 命中规则的歌才在所有列表里隐藏、在队列与电台里跳过。

use serde::{Deserialize, Serialize};

/// 按什么屏蔽。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// 歌手名。曲目上只带歌手名、不带歌手 id,所以按名字认。
    Artist,
    /// 账号级标签名(#158)。
    Tag,
    /// 单曲,值是平台内的曲目 id。
    Track,
}

/// 一条屏蔽规则。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct BlockRuleDto {
    pub id: String,
    pub kind: BlockKind,
    pub value: String,
    /// 设置页「已屏蔽」里显示的那一行。单曲的 `value` 是一串数字,
    /// 得靠它说出是哪首歌;歌手与标签就是 `value` 本身。
    pub label: String,
}

/// `POST /blocks` 的请求体。`label` 不给就用 `value`。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct NewBlockRuleDto {
    pub kind: BlockKind,
    pub value: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// `GET /blocks` 的响应体。
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize,
)]
pub struct BlockRulesDto {
    pub rules: Vec<BlockRuleDto>,
}
