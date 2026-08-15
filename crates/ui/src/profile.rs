//! 个人主页:收听统计的取与摆。
//!
//! 数据来自 `api::stats()`(服务端从播放事件流查询时聚合)。每次进页都重取:
//! 统计本来就该反映"到现在为止",缓存一份旧的还要管失效,不值。
//! 取不回来只在页面上留一句话,不拦别的功能。

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::Profile;
use crate::{ArtistRankRow, MainWindow};

pub(crate) fn bind(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.global::<Profile>().on_shown(move || {
        let weak = weak.clone();
        let _ = slint::spawn_local(async move {
            let result = api::stats().await;
            let Some(ui) = weak.upgrade() else { return };
            match result {
                Ok(stats) => show(&ui, &stats),
                Err(_) => {
                    // 细节进不了一行字,横幅归断流那类大事;这里只说结果。
                    ui.global::<Profile>()
                        .set_error("查询失败".into());
                }
            }
        });
    });
}

/// 把一份统计摆上页面。数字在这里格式化,`.slint` 不做计算。
fn show(ui: &MainWindow, stats: &api::StatsDto) {
    ui.global::<Profile>()
        .set_username(stats.username.as_str().into());
    ui.global::<Profile>().set_month_plays(
        format!("{} 次", stats.month_plays).into(),
    );
    ui.global::<Profile>().set_distinct_tracks(
        format!("{} 首", stats.distinct_tracks).into(),
    );
    ui.global::<Profile>().set_streak_days(
        format!("{} 天", stats.streak_days).into(),
    );

    // 条长按榜首归一:榜单要读的是相对多少,不是绝对次数(那个在右侧数字上)。
    let most = stats
        .top_artists
        .first()
        .map_or(1, |artist| artist.plays.max(1));
    let rows: Vec<ArtistRankRow> = stats
        .top_artists
        .iter()
        .map(|artist| ArtistRankRow {
            name: artist.name.as_str().into(),
            plays_text: format!("{} 次", artist.plays)
                .into(),
            ratio: artist.plays as f32 / most as f32,
        })
        .collect();
    ui.global::<Profile>()
        .set_artists(ModelRc::new(VecModel::from(rows)));

    ui.global::<Profile>()
        .set_error(slint::SharedString::new());
    ui.global::<Profile>().set_loaded(true);
}
