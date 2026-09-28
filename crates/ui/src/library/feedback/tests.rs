use super::*;

fn window() -> MainWindow {
    i_slint_backend_testing::init_no_event_loop();
    MainWindow::new().expect("建不出主窗口")
}

fn now_feedback(ui: &MainWindow) -> i32 {
    ui.global::<Player>().get_now_feedback()
}

/// 点赞 → 再点一次赞(取消)→ 改点踩(覆盖)。与服务端 #157 的验收步骤同形。
#[test]
fn toggling_sets_cancels_and_overwrites() {
    let ui = window();
    bind(&ui);
    ui.global::<Player>().set_now_id("1".into());

    ui.global::<Library>().invoke_toggle_feedback(1);
    assert_eq!(now_feedback(&ui), 1, "点赞该当场亮");

    ui.global::<Library>().invoke_toggle_feedback(1);
    assert_eq!(now_feedback(&ui), 0, "再点一次该取消");

    ui.global::<Library>().invoke_toggle_feedback(-1);
    assert_eq!(now_feedback(&ui), -1, "改点踩该覆盖");

    ui.global::<Library>().invoke_toggle_feedback(-1);
    assert_eq!(now_feedback(&ui), 0, "再点一次踩也该取消");
}

/// 手上没有正在放的歌,点键不该动 `now-feedback`。
#[test]
fn toggling_without_a_track_is_a_noop() {
    let ui = window();
    bind(&ui);

    ui.global::<Library>().invoke_toggle_feedback(1);
    assert_eq!(now_feedback(&ui), 0);
}

/// 手上没歌时,`refresh` 直接清成 0,不发请求。
#[test]
fn refresh_with_an_empty_id_clears_without_asking() {
    let ui = window();
    ui.global::<Player>().set_now_feedback(1);

    refresh(&ui, "");

    assert_eq!(now_feedback(&ui), 0);
}
