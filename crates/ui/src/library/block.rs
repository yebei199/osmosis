//! 屏蔽规则的客户端侧(#161):手上一份规则,队列前进时拿它跳过命中的歌;
//! 歌曲菜单、歌手页的「屏蔽」与设置页「已屏蔽」的「恢复」也在这里接。
//!
//! 列表的隐藏在服务端出口做,这里不重复:建、删之后把当前视图重取一遍就是。
//! 这份规则只管**已经装进队列**的那一批 —— 它们是规则建立之前拿到的。

use std::cell::RefCell;
use std::rc::Rc;

use app_core::{BlockKind, BlockRuleDto, TrackDto};
use slint::{ComponentHandle, VecModel};

use crate::BlockRow;
use crate::Library;
use crate::MainWindow;

/// 这个账号的屏蔽规则。启动时、进设置页时拉一次,本机建、删之后就地改。
pub type BlockSet = Rc<RefCell<Vec<BlockRuleDto>>>;

/// 这首歌命不命中手上的规则。
pub fn hits(set: &BlockSet, track: &TrackDto) -> bool {
    app_core::blocks::hits(&set.borrow(), track)
}

/// 接上建、删、重拉,并先拉一次。`reload` 把当前视图重取一遍 —— 视图归
/// `crate::music`,这边只喊它。
pub fn bind(
    ui: &MainWindow,
    set: &BlockSet,
    reload: impl Fn(&MainWindow) + Clone + 'static,
) {
    refresh(set, ui);

    let (weak, held) = (ui.as_weak(), set.clone());
    ui.global::<Library>().on_refresh_blocks(move || {
        if let Some(ui) = weak.upgrade() {
            refresh(&held, &ui);
        }
    });

    let (weak, held, again) =
        (ui.as_weak(), set.clone(), reload.clone());
    ui.global::<Library>().on_block_track(
        move |id, title| {
            create(
                &weak,
                &held,
                again.clone(),
                BlockKind::Track,
                id.to_string(),
                title.to_string(),
            );
        },
    );

    let (weak, held, again) =
        (ui.as_weak(), set.clone(), reload.clone());
    ui.global::<Library>().on_block_artist(move |name| {
        create(
            &weak,
            &held,
            again.clone(),
            BlockKind::Artist,
            name.to_string(),
            name.to_string(),
        );
    });

    let (weak, held, again) =
        (ui.as_weak(), set.clone(), reload.clone());
    ui.global::<Library>().on_block_tag(move |name| {
        create(
            &weak,
            &held,
            again.clone(),
            BlockKind::Tag,
            name.to_string(),
            name.to_string(),
        );
    });

    let (weak, held) = (ui.as_weak(), set.clone());
    ui.global::<Library>().on_unblock(move |id| {
        let (weak, held, again) =
            (weak.clone(), held.clone(), reload.clone());
        let id = id.to_string();
        let _ = slint::spawn_local(async move {
            let done = api::delete_block(&id).await;
            let Some(ui) = weak.upgrade() else { return };
            match done {
                Ok(()) => {
                    held.borrow_mut()
                        .retain(|rule| rule.id != id);
                    project(&held, &ui);
                    again(&ui);
                }
                Err(err) => report(&ui, &err, "恢复失败"),
            }
        });
    });
}

/// 设置页「已屏蔽」那一列的行,先建的在前。
pub fn to_rows(rules: &[BlockRuleDto]) -> Vec<BlockRow> {
    rules
        .iter()
        .map(|rule| BlockRow {
            id: rule.id.as_str().into(),
            label: rule.label.as_str().into(),
            kind: kind_text(rule.kind).into(),
        })
        .collect()
}

fn kind_text(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Artist => "歌手",
        BlockKind::Tag => "标签",
        BlockKind::Track => "单曲",
    }
}

/// 拉一次全部规则。失败只写日志:拿不到规则只是队列不跳过,不该挡住听歌。
fn refresh(set: &BlockSet, ui: &MainWindow) {
    let (set, weak) = (set.clone(), ui.as_weak());
    let _ = slint::spawn_local(async move {
        match api::blocks().await {
            Ok(dto) => {
                *set.borrow_mut() = dto.rules;
                if let Some(ui) = weak.upgrade() {
                    project(&set, &ui);
                }
            }
            Err(err) => log::warn!("取屏蔽规则失败: {err}"),
        }
    });
}

/// 建一条规则:成了就记进手上那份、提示一声、重取当前视图。
fn create(
    weak: &slint::Weak<MainWindow>,
    set: &BlockSet,
    reload: impl Fn(&MainWindow) + 'static,
    kind: BlockKind,
    value: String,
    label: String,
) {
    let (weak, set) = (weak.clone(), set.clone());
    let _ = slint::spawn_local(async move {
        let created =
            api::create_block(kind, &value, &label).await;
        let Some(ui) = weak.upgrade() else { return };
        match created {
            Ok(rule) => {
                let text = format!(
                    "已屏蔽{}「{}」,可在设置里恢复",
                    kind_text(rule.kind),
                    rule.label
                );
                let mut held = set.borrow_mut();
                if !held
                    .iter()
                    .any(|known| known.id == rule.id)
                {
                    held.push(rule);
                }
                drop(held);
                project(&set, &ui);
                crate::notice::show(&ui, text);
                reload(&ui);
            }
            Err(err) => report(&ui, &err, "屏蔽失败"),
        }
    });
}

fn project(set: &BlockSet, ui: &MainWindow) {
    ui.global::<Library>().set_blocks(slint::ModelRc::new(
        VecModel::from(to_rows(&set.borrow())),
    ));
}

/// 报一次失败。走横幅,不走播放状态行(见 `crate::notice`)。
fn report(
    ui: &MainWindow,
    err: &api::ApiError,
    what: &str,
) {
    if crate::pages::account::handle_session_expiry(ui, err)
    {
        return;
    }
    crate::notice::show(ui, format!("{what}: {err}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 设置页的每一行带上类别字样,单曲显示标题而不是那串 id。
    #[test]
    fn rows_name_the_kind_and_show_the_label() {
        let rows = to_rows(&[
            BlockRuleDto {
                id: "1".to_owned(),
                kind: BlockKind::Track,
                value: "123456".to_owned(),
                label: "晴天".to_owned(),
            },
            BlockRuleDto {
                id: "2".to_owned(),
                kind: BlockKind::Artist,
                value: "某人".to_owned(),
                label: "某人".to_owned(),
            },
        ]);

        assert_eq!(rows[0].label, "晴天");
        assert_eq!(rows[0].kind, "单曲");
        assert_eq!(rows[1].kind, "歌手");
    }
}
