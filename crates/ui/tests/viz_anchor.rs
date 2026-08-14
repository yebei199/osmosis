//! 播放页标注卡的挂载:卡片跟着 seam 回传的视口锚点走,遮挡层裁到卡片自己。
//!
//! 投影在 render3d 那侧(有它自己的单测),这里钉的是 UI 这一半:锚点属性一变
//! 卡片有没有跟着挪、不可见时在不在元素树里、遮挡层那张图有没有铺出卡片之外。
//! 铺出去就会把整幅点云糊在播放页上,而那是最刺眼的一种错画面。

use i_slint_backend_testing as testing;
use slint::ComponentHandle;
use ui::MainWindow;
use ui::Viz;

/// 播放页展开的窗口。标注卡是播放页里的条件元素,页不开它不实例化。
fn play_window() -> MainWindow {
    testing::init_no_event_loop();
    let ui = MainWindow::new().expect("建不出主窗口");
    ui.window()
        .set_size(slint::LogicalSize::new(400.0, 800.0));
    ui.set_play_page_open(true);
    ui.global::<Viz>().set_now_title("锚定测试曲".into());
    ui.global::<Viz>().set_viz_anchor_x(0.5);
    ui.global::<Viz>().set_viz_anchor_y(0.5);
    ui.global::<Viz>().set_viz_anchor_visible(true);
    ui
}

/// 窗口的逻辑尺寸。锚点是归一化的,换算回像素要乘它。
fn window_size(ui: &MainWindow) -> (f32, f32) {
    let size = ui
        .window()
        .size()
        .to_logical(ui.window().scale_factor());
    (size.width, size.height)
}

/// 按元素 id 取一个元素。条件元素不成立时给 `None`。
fn element(
    ui: &MainWindow,
    id: &str,
) -> Option<testing::ElementHandle> {
    testing::ElementHandle::find_by_element_id(ui, id)
        .next()
}

fn card(ui: &MainWindow) -> Option<testing::ElementHandle> {
    element(ui, "MainWindow::viz-anchor-card")
}

fn occluder(
    ui: &MainWindow,
) -> Option<testing::ElementHandle> {
    element(ui, "MainWindow::viz-anchor-occluder")
}

/// 一张非空的图。遮挡层那一层的守卫是 `width > 0`,空图不实例化。
fn filled_image(w: u32, h: u32) -> slint::Image {
    slint::Image::from_rgba8(slint::SharedPixelBuffer::<
        slint::Rgba8Pixel,
    >::new(w, h))
}

/// 锚点在视口里横竖各走一趟,卡片跟着走,且是**中心**挂在锚点上。
/// 挂左上角的话卡片会整体偏出去半个身位,标注就指不准物体了。
#[test]
fn the_card_follows_the_anchor_across_the_viewport() {
    let ui = play_window();
    let (w, h) = window_size(&ui);

    for (ax, ay) in [(0.5, 0.5), (0.25, 0.75), (0.8, 0.2)] {
        ui.global::<Viz>().set_viz_anchor_x(ax);
        ui.global::<Viz>().set_viz_anchor_y(ay);

        let card = card(&ui).expect("锚点可见时该有标注卡");
        let pos = card.absolute_position();
        let size = card.size();
        let center = (
            pos.x + size.width / 2.0,
            pos.y + size.height / 2.0,
        );
        assert!(
            (center.0 - ax * w).abs() < 0.5,
            "锚点 x = {ax} 时卡片中心该在 {},实际 {}",
            ax * w,
            center.0
        );
        assert!(
            (center.1 - ay * h).abs() < 0.5,
            "锚点 y = {ay} 时卡片中心该在 {},实际 {}",
            ay * h,
            center.1
        );
    }
}

/// 锚点贴到画面边上时,卡片整块仍在窗口内。
///
/// 锚点在画面里不等于卡片在画面里:锚点是卡片的**中心**,它贴着边时卡片已经
/// 探出去半个身位(真机实拍切掉过右边约 20%)。render3d 那侧按竖屏算过轨道
/// 半径,但窗口宽度是 UI 这边的事,得在这里兜住。
#[test]
fn the_card_stays_inside_the_window_at_the_edges() {
    let ui = play_window();
    let (w, h) = window_size(&ui);

    for (ax, ay) in [(0.0, 0.0), (1.0, 1.0), (1.0, 0.0)] {
        ui.global::<Viz>().set_viz_anchor_x(ax);
        ui.global::<Viz>().set_viz_anchor_y(ay);

        let card = card(&ui).expect("锚点可见时该有标注卡");
        let pos = card.absolute_position();
        let size = card.size();
        assert!(
            pos.x >= 0.0 && pos.x + size.width <= w,
            "锚点 x = {ax} 时卡片横向探出窗口:{}..{},窗口宽 {w}",
            pos.x,
            pos.x + size.width
        );
        assert!(
            pos.y >= 0.0 && pos.y + size.height <= h,
            "锚点 y = {ay} 时卡片纵向探出窗口:{}..{},窗口高 {h}",
            pos.y,
            pos.y + size.height
        );
    }
}

/// 锚点不可见(转到画面外或相机背后)时,卡片整个不在元素树里。
/// 只是挪到界外不算数:界外的元素照样参与布局与命中测试。
#[test]
fn the_card_leaves_the_tree_when_the_anchor_is_not_visible()
{
    let ui = play_window();
    assert!(card(&ui).is_some(), "锚点可见时它该在");

    ui.global::<Viz>().set_viz_anchor_visible(false);
    assert!(
        card(&ui).is_none(),
        "锚点不可见时卡片该整块离开元素树"
    );
}

/// 遮挡层裁到卡片矩形:那张图铺的是整个视口,但只有卡片这一块该露出来。
/// 不裁就是把整幅点云盖在播放页上。
#[test]
fn the_occluder_is_clipped_to_the_card() {
    let ui = play_window();
    ui.global::<Viz>()
        .set_viz_occluder(filled_image(64, 64));

    let card = card(&ui).expect("锚点可见时该有标注卡");
    let clip =
        occluder(&ui).expect("有遮挡层图时该有裁剪层");
    assert_eq!(
        clip.absolute_position(),
        card.absolute_position(),
        "裁剪层要与卡片同位"
    );
    assert_eq!(
        clip.size(),
        card.size(),
        "裁剪层要与卡片同大 —— 大出去的部分就是糊在播放页上的点云"
    );
}

/// 没有遮挡层图(无 GPU 的端、或纹理还没就绪)时,卡片照常显示、不被裁没。
/// 空图守卫走的是 `width > 0`,与 viz-scene 同一套路。
#[test]
fn the_card_shows_without_an_occluder_image() {
    let ui = play_window();
    assert!(card(&ui).is_some(), "没有遮挡层图也该有卡片");
    assert!(
        occluder(&ui).is_none(),
        "空图时那一层不该实例化"
    );
}
