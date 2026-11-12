//! 播放页的效果:封面实物与封面点云,二者择一。
//!
//! 「效果」是播放页 3D 那一层的顶层选择,两个效果各是一摊自己的实体与材质;
//! 只有当前那一个可见、被渲,另一个整摊藏起来(`Visibility::Hidden`),一帧不画。
//! 换效果不重建任何东西,两摊都常驻,封面也两边都收着(见 `Scene::apply_cover`)。

/// 播放页的效果。编号是跨层的 seam:`.slint` 的 `viz-effect`、`ui::viz::effect`
/// 的落盘映射与这里手工对齐,按下标说话。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Effect {
    /// 封面实物:一块带倒角的薄封套,下面一片暗色亮面地板与倒影。默认档。
    #[default]
    Sleeve,
    /// 封面点云:封面采样成九千多颗方块,随声音起伏。
    Cloud,
}

impl Effect {
    /// 界面给的编号 → 效果。越界回默认档。
    ///
    /// 编号来自 `.slint` 与设置文件,是跨层的外部输入;给个不存在的档不该画出一片空白。
    pub(crate) fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Cloud,
            _ => Self::Sleeve,
        }
    }
}

#[cfg(test)]
mod tests;
