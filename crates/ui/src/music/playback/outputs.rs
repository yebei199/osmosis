//! 输出设备那一排:选一台、「+」加入 / 移出(#142)。
//!
//! 都只改服务端全局状态里的出声设备(`POST /group/outputs`),不再有迁移的三步编排:
//! 新加入的那台看到状态里有自己,就按标识取执行副本、到点跟上;被移出的那台看到没有自己,
//! 就停下。本机还独奏着时选了别的设备,组从本机正在放的那一份接着放(`seed`);组已经在
//! 别处放着时则只往它的出声设备里加,原有的不动(#149)。已有组也带有效本机种子,
//! 服务端仅在清掉失效播放后采用,用于电台接续(#174)。

use app_core::{GroupSeedDto, PlaybackState};

use super::*;
use crate::Shell;
use crate::music::*;

/// 点一台设备:改成只在它上面出声。空串是本机。
pub(in crate::music) fn select_output(
    ui: &MainWindow,
    deck: &Deck,
    id: &str,
) {
    let target = device_of(deck, id);
    if deck.group.is_member() {
        // 种子是可选的恢复材料;尚未同步时保留正常组切输出的行为。
        deck.group.set_outputs(
            ui,
            vec![target],
            local_seed(ui, deck).ok().flatten(),
        );
        return;
    }
    // 独奏时点本机:本来就在本机。
    if target == deck.group.me() {
        return;
    }
    // 组已经在放:加进去,不拿本机播放建一个新的把原有的挤掉(#149)。
    if deck
        .group
        .state()
        .is_some_and(|state| state.now.is_some())
        && deck.group.add_output(
            ui,
            &target,
            local_seed(ui, deck).ok().flatten(),
        )
    {
        return;
    }
    start_group(ui, deck, vec![target]);
}

/// 「+」加入 / 移出:在正在出声的那几台上加上或去掉这一台。空串是本机。
pub(in crate::music) fn toggle_member(
    ui: &MainWindow,
    deck: &Deck,
    id: &str,
) {
    let target = device_of(deck, id);
    let me = deck.group.me().to_owned();
    // 独奏、组已经在放:只往组里加,不移出 —— 独奏时芯片上组的设备不亮,那颗键写的是
    // 「加入」(#149)。本机那颗照旧:独奏时它本来就在出声。
    if !deck.group.is_member()
        && target != me
        && deck
            .group
            .state()
            .is_some_and(|state| state.now.is_some())
        && deck.group.add_output(
            ui,
            &target,
            local_seed(ui, deck).ok().flatten(),
        )
    {
        return;
    }
    let mut outputs = if deck.group.is_member() {
        deck.group
            .state()
            .map(|state| state.outputs)
            .unwrap_or_default()
    } else if local_sounding(deck) {
        vec![me.clone()]
    } else {
        Vec::new()
    };
    let before = outputs.len();
    outputs.retain(|output| *output != target);
    if outputs.len() == before {
        outputs.push(target);
    }
    if deck.group.is_member() {
        deck.group.set_outputs(
            ui,
            outputs,
            local_seed(ui, deck).ok().flatten(),
        );
        return;
    }
    // 独奏时只动了本机:还是独奏,不必建组。
    if outputs.iter().all(|output| *output == me) {
        return;
    }
    start_group(ui, deck, outputs);
}

/// 从独奏建组:本机正在放的那一份当种子,组接着放。
fn start_group(
    ui: &MainWindow,
    deck: &Deck,
    outputs: Vec<String>,
) {
    match local_seed(ui, deck) {
        Ok(seed) => {
            deck.group.set_outputs(ui, outputs, seed)
        }
        Err(why) => crate::notice::show(ui, why),
    }
}

/// 芯片上的 id 换成设备 id:空串是本机。
fn device_of(deck: &Deck, id: &str) -> String {
    if id.is_empty() {
        deck.group.me().to_owned()
    } else {
        id.to_owned()
    }
}

/// 本机播放器此刻在不在出声。
fn local_sounding(deck: &Deck) -> bool {
    deck.player.as_ref().as_ref().is_ok_and(is_sounding)
}

/// 本机正在放的那一份。什么都没放是 `Ok(None)`:组从空的开始,不是错。
///
/// 这一批还没同步到服务端时建不了:别的设备只能按服务端的标识取执行副本。
fn local_seed(
    ui: &MainWindow,
    deck: &Deck,
) -> Result<Option<GroupSeedDto>, String> {
    let state = deck.playback.borrow().state().clone();
    if !matches!(
        state,
        PlaybackState::Playing(_)
            | PlaybackState::Loading(_)
    ) {
        return Ok(None);
    }
    let index = deck.queue.borrow().index();
    let (queue_id, _, applied) = deck.execution.identity();
    let entry = deck.execution.entry_at(index);
    let (Some(queue_id), Some(revision), Some(entry_id)) =
        (queue_id, applied, entry)
    else {
        // 顺手补一次同步:服务端回来了的话,下一次按就建得起来了。
        resync_local_queue(ui, deck);
        return Err("本机这一批还没同步到服务端,稍后再切"
            .to_owned());
    };
    let player = deck.player.as_ref().as_ref().ok();
    Ok(Some(GroupSeedDto {
        queue_id,
        revision,
        entry_id,
        position_ms: player.map_or(0, |player| {
            player.position().as_millis() as u64
        }),
        playing: player.is_some_and(is_sounding),
    }))
}

/// 本机播放器此刻在不在出声:没按暂停、手上也有源。
///
/// 播放逻辑问这里,不问界面上的 ⏸/▶ —— 界面是投影(#137 ③)。
pub(in crate::music) fn is_sounding(
    player: &audio::Player,
) -> bool {
    !player.is_paused() && !player.empty()
}

/// 输出设备那一排的两种点法。
pub(in crate::music) fn bind_outputs(
    ui: &MainWindow,
    deck: &Deck,
) {
    let selecting = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_set_output(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        select_output(&ui, &selecting, &id);
    });

    let toggling = deck.clone();
    let weak = ui.as_weak();
    ui.global::<Shell>().on_toggle_member(move |id| {
        let Some(ui) = weak.upgrade() else { return };
        toggle_member(&ui, &toggling, &id);
    });
}
