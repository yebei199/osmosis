use super::*;
use camera::YAW_MAX;

const N: usize = 30;

fn wide() -> WallLayout {
    layout(1808.0, 864.0, false, N)
}

fn poses(lay: &WallLayout, n: usize) -> Vec<CardPose> {
    (0..n).map(|i| card_pose(lay, i, 1.0)).collect()
}

/// 布局全部按容器尺寸推导:同比放大容器,布局同比放大。
#[test]
fn layout_scales_with_container() {
    let a = layout(904.0, 432.0, false, N);
    let b = layout(1808.0, 864.0, false, N);
    assert!((b.card - a.card * 2.0).abs() < 0.01);
    assert!((b.col_pitch - a.col_pitch * 2.0).abs() < 0.01);
    assert!((b.row_pitch - a.row_pitch * 2.0).abs() < 0.01);
    // 参考尺寸下卡取回设计稿原值。列距不再是设计稿的 204:环面周期要罩住
    // 最远那层的视口,撑出来的间距总比它大,204 只剩「不许更密」的下限意义。
    assert!((a.card - 150.0).abs() < 0.01);
    assert!(a.col_pitch >= 204.0);
}

/// 紧凑版式卡更小,一屏上大约两列 —— 手机上再密就点不准了。
#[test]
fn compact_layout_fits_two_columns() {
    let lay = layout(420.0, 700.0, true, N);
    let on_screen = lay.w / lay.col_pitch;
    assert!(
        (1.5..3.0).contains(&on_screen),
        "一屏 {on_screen:.1} 列,不是约两列"
    );
    assert!(lay.card < 90.0);
}

/// 网格形状跟着场区长宽比走:横场列多于行,竖场反过来。
/// 这是「近方形自适应」在非方形场区上的样子 —— 死取方形会在竖屏上
/// 把三十张卡压成一条横带。
#[test]
fn grid_shape_follows_the_field_aspect() {
    let wide = layout(1808.0, 700.0, false, N);
    assert!(
        wide.cols > wide.rows,
        "横场该列多:{}×{}",
        wide.rows,
        wide.cols
    );
    let tall = layout(420.0, 1400.0, true, N);
    assert!(
        tall.rows > tall.cols,
        "竖场该行多:{}×{}",
        tall.rows,
        tall.cols
    );
}

/// 环面周期必须罩得住**最远那层卡**上的视口:按 z=0 那层算的话,远处的卡
/// 会在屏幕里回绕(见 dragging_never_makes_a_visible_card_jump)。
#[test]
fn wrap_span_covers_the_viewport() {
    for (w, h, compact, n) in [
        (1808.0, 864.0, false, 30),
        (904.0, 432.0, false, 36),
        (420.0, 1400.0, true, 30),
        (420.0, 1400.0, true, 4),
    ] {
        let lay = layout(w, h, compact, n);
        let far = 1.0 + (-lay.z_min) / lay.perspective;
        assert!(
            lay.span_x() >= w * far + lay.card * 2.0 - 0.01,
            "x 周期不够罩住 {w}×{h} 的最远层"
        );
        assert!(
            lay.span_y() >= h * far + lay.card * 2.0 - 0.01,
            "y 周期不够罩住 {w}×{h} 的最远层"
        );
    }
}

/// 散布确定性:同一张卡两次求位姿逐位相等,卡墙不许闪。
#[test]
fn poses_are_deterministic() {
    let lay = wide();
    assert_eq!(
        card_pose(&lay, 7, 1.0),
        card_pose(&lay, 7, 1.0)
    );
}

/// 卡墙态的散布落在设计稿范围内;塌回态深度与旋转全部归零。
#[test]
fn collapse_flattens_every_card() {
    let lay = wide();
    for i in 0..N {
        let wall = card_pose(&lay, i, 1.0);
        assert!(
            wall.z >= lay.z_min - 0.01
                && wall.z <= lay.z_max + 0.01
        );
        assert!(wall.rot_y.to_degrees() >= -16.1);
        assert!(wall.rot_y.to_degrees() <= 14.1);

        let flat = card_pose(&lay, i, 0.0);
        assert_eq!(flat.z, 0.0);
        assert_eq!(flat.rot_y, 0.0);
        assert_eq!(flat.rot_x, 0.0);
        assert_eq!(flat.dim, 1.0);
        // 平面位置不随塌回改变:同一批卡只是深度插值。
        assert_eq!(flat.x, wall.x);
        assert_eq!(flat.y, wall.y);
    }
}

/// 双向无限:整整拖过一个周期,每张卡回到原处 —— 环面上「拖到底」
/// 这件事不存在。
#[test]
fn panning_a_whole_period_returns_the_same_wall() {
    let lay = wide();
    let ps = poses(&lay, N);
    let home = WallCam::default();
    let mut moved = WallCam::default();
    moved.pan_x = lay.span_x();
    moved.pan_y = lay.span_y();
    for p in &ps {
        let a = world_pose(&lay, &home, p);
        let b = world_pose(&lay, &moved, p);
        assert!(
            (a.x - b.x).abs() < 0.05
                && (a.y - b.y).abs() < 0.05,
            "拖过一个周期后没回到原处"
        );
    }
}

/// 往任意方向拖多远,卡都还在环面的中心格里 —— 不会有「拖出去就空了」
/// 的一边。竖向同理,这正是这次要补的那一轴。
#[test]
fn cards_stay_around_the_camera_however_far_you_pan() {
    let lay = wide();
    let ps = poses(&lay, N);
    for (px, py) in
        [(0.0, 0.0), (5e4, 0.0), (0.0, -3e4), (-7e4, 9e4)]
    {
        let mut cam = WallCam::default();
        cam.pan_x = px;
        cam.pan_y = py;
        let mut on_screen = 0;
        for p in &ps {
            let w = world_pose(&lay, &cam, p);
            if let Some((sx, sy, _)) = project(&lay, &w)
                && (0.0..lay.w).contains(&sx)
                && (0.0..lay.h).contains(&sy)
            {
                on_screen += 1;
            }
        }
        assert!(
            on_screen >= 6,
            "平移到 ({px}, {py}) 后屏幕上只剩 {on_screen} 张卡"
        );
    }
}

/// 松手后惯性衰减,最终必须停;不再吸附格点(停在哪都成立)。
#[test]
fn released_camera_coasts_to_a_stop() {
    let mut cam = WallCam::default();
    cam.drag(-90.0, 40.0);
    assert!(cam.step());
    cam.release();
    for _ in 0..300 {
        cam.step();
    }
    assert!(!cam.step(), "300 帧后还在动,惯性不收敛");
    assert_eq!(cam.yaw, 0.0);
    assert_eq!(cam.pitch, 0.0);
}

/// 帧率高于鼠标上报率时,一半的帧收不到新指针事件、位移是 0。姿态角
/// 不许因此逐帧在满偏与归零之间跳 —— 那正是「拖着抖」的样子。
#[test]
fn empty_pointer_frames_do_not_shake_the_wall() {
    let mut cam = WallCam::default();
    let mut angles = Vec::new();
    for frame in 0..40 {
        // 偶数帧有事件、奇数帧没有:120Hz 屏配 60Hz 鼠标。
        cam.drag(
            if frame % 2 == 0 { -24.0 } else { 0.0 },
            0.0,
        );
        cam.step();
        angles.push(cam.yaw);
    }
    // 稳下来之后,相邻两帧的姿态差远小于满偏。
    let jump = angles
        .windows(2)
        .skip(20)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        jump < YAW_MAX * 0.1,
        "空帧让整墙来回甩:相邻帧 yaw 差 {jump}"
    );
    // 而且确实偏出去了,不是被平滑成一动不动。
    assert!(angles.last().unwrap().abs() > 0.02);
}

/// 松手前那一帧恰好没有指针事件时,惯性不许整个丢掉。
#[test]
fn a_stalled_last_frame_still_coasts() {
    let mut cam = WallCam::default();
    for _ in 0..10 {
        cam.drag(-24.0, 12.0);
        cam.step();
    }
    // 最后一帧没有新事件。
    cam.drag(0.0, 0.0);
    cam.step();
    cam.release();
    let (x0, y0) = (cam.pan_x, cam.pan_y);
    for _ in 0..20 {
        cam.step();
    }
    assert!(
        (cam.pan_x - x0).abs() > 10.0
            && (cam.pan_y - y0).abs() > 5.0,
        "松手后没有滑行:Δ = ({}, {})",
        cam.pan_x - x0,
        cam.pan_y - y0
    );
}

/// 稳定拖动时,屏幕上不许有**看得见的卡**发生跳变。
///
/// 这是「抖动」的判据,比盯某个角度靠谱:卡在屏幕上的位置是 yaw、pitch、
/// 回绕、投影缩放共同的结果,任何一环出问题都会在这里显形。环面回绕本身
/// 就是跳变 —— 但那一跳必须发生在视口外,看得见的回绕就是抖。
#[test]
fn dragging_never_makes_a_visible_card_jump() {
    for (w, h, compact, n) in [
        (1638.0, 1866.0, false, 30),
        (1808.0, 864.0, false, 30),
        (420.0, 1400.0, true, 30),
    ] {
        for (dx, dy) in
            [(-STEP, 0.0), (0.0, -STEP), (STEP, STEP)]
        {
            check_no_visible_jump(w, h, compact, n, dx, dy);
        }
    }
}

/// 每帧的指针位移。稳定拖动时,卡在屏幕上该跟着走同一个量级。
const STEP: f32 = 12.0;

/// 一次拖动的逐帧检查,见 [`dragging_never_makes_a_visible_card_jump`]。
fn check_no_visible_jump(
    w: f32,
    h: f32,
    compact: bool,
    n: usize,
    dx: f32,
    dy: f32,
) {
    let lay = layout(w, h, compact, n);
    let ps = poses(&lay, n);
    let mut cam = WallCam::default();
    // 每张卡上一帧的 (屏幕 x, 屏幕 y, 半边长)。
    let mut prev: Vec<Option<(f32, f32, f32)>> =
        vec![None; n];
    // 姿态角先跑稳,再开始判定:起手那几帧 yaw/pitch 本来就在爬。
    let settle = 60;
    for frame in 0..400 {
        cam.drag(dx, dy);
        cam.step();
        for (i, p) in ps.iter().enumerate() {
            let world = world_pose(&lay, &cam, p);
            let now = project(&lay, &world);
            if let (Some(a), Some(b), true) =
                (prev[i], now, frame > settle)
            {
                let moved = ((b.0 - a.0).powi(2)
                    + (b.1 - a.1).powi(2))
                .sqrt();
                // 宽到 4 倍指针位移:投影放大与姿态微调都算正常。
                if moved > STEP * 4.0 {
                    assert!(
                        offscreen(&lay, a)
                            && offscreen(&lay, b),
                        "{w}×{h} 拖 ({dx}, {dy}):第 {frame} 帧卡 {i} \
                         在屏幕上跳了 {moved:.0}px,{a:?} → {b:?}"
                    );
                }
            }
            prev[i] = now;
        }
    }
}

/// 这张卡整个落在视口外吗(含它自己的半边长)。
fn offscreen(
    lay: &WallLayout,
    (x, y, half): (f32, f32, f32),
) -> bool {
    x + half < 0.0
        || x - half > lay.w
        || y + half < 0.0
        || y - half > lay.h
}

/// 拖动两轴都平移:竖向不再只是俯仰。
#[test]
fn drag_pans_both_axes() {
    let mut cam = WallCam::default();
    cam.drag(-30.0, -50.0);
    // 平移吃满位移,一帧不差 —— 平滑只作用在速度上。
    assert!((cam.pan_x - 30.0).abs() < 0.01);
    assert!((cam.pan_y - 50.0).abs() < 0.01);
}

/// 滚轮就是竖着划一下:内容跟着滚,镜头深度不动。
#[test]
fn wheel_scrolls_vertically() {
    let mut cam = WallCam::default();
    let before = cam.pan_x;
    cam.wheel(-120.0);
    assert!(cam.pan_y > 0.0, "滚轮下滑该让内容往上走");
    assert!(
        cam.pan_y > 100.0,
        "滚轮的平移量该吃满,不该被平滑削掉"
    );
    assert_eq!(cam.pan_x, before, "滚轮不该动横轴");
}

/// 命中测试:把某张卡投到屏幕,再拿投影中心去问,要拿回同一张。
#[test]
fn hit_test_finds_the_projected_card() {
    let lay = wide();
    let cam = WallCam::default();
    let ps = poses(&lay, 12);
    let world = world_pose(&lay, &cam, &ps[4]);
    let (sx, sy, _) = project(&lay, &world).unwrap();
    let hit = hit_test(&lay, &cam, &ps, sx, sy).unwrap();
    // 命中的要么是它自己,要么是遮在它前面(z 更大)的卡。
    assert!(world_pose(&lay, &cam, &ps[hit]).z >= world.z);
}

/// 命中测试跟着回绕走:拖过一个周期之后,同一个屏幕点还是同一张卡。
#[test]
fn hit_test_follows_the_wrap() {
    let lay = wide();
    let ps = poses(&lay, N);
    let home = WallCam::default();
    let world = world_pose(&lay, &home, &ps[9]);
    let (sx, sy, _) = project(&lay, &world).unwrap();
    let mut moved = WallCam::default();
    moved.pan_x = -lay.span_x() * 2.0;
    moved.pan_y = lay.span_y() * 3.0;
    assert_eq!(
        hit_test(&lay, &home, &ps, sx, sy),
        hit_test(&lay, &moved, &ps, sx, sy)
    );
}

/// 塌回与 dolly 都在有限帧内收敛(420ms ≈ 25 帧的量级)。
#[test]
fn transitions_settle_within_a_second() {
    let mut c = Collapse {
        target: 0.0,
        ..Collapse::default()
    };
    let mut frames = 0;
    while c.step() {
        frames += 1;
        assert!(frames < 60, "塌回一秒都收不住");
    }

    let mut d = DollyRun {
        t: 0.0,
        target_z: 40.0,
    };
    let mut frames = 0;
    while !d.step() {
        frames += 1;
        assert!(frames < 60, "dolly 一秒都落不了位");
    }
}

/// 卡面烘焙:四周撑出投影余量,角上透明、中心不动、边缘提亮成描边。
#[test]
fn card_bake_adds_rounded_corners_border_and_shadow() {
    let (w, h) = (64u32, 64u32);
    // 中灰,好让描边的提亮看得出来。
    let src = vec![128u8; (w * h * 4) as usize];
    let (out, ow, oh) = bake_card(&src, w, h);
    assert_eq!(
        (ow, oh),
        (w + 20, h + 20),
        "四周该各留 10px"
    );
    assert_eq!(out.len(), (ow * oh * 4) as usize);

    let at = |x: u32, y: u32| {
        let i = ((y * ow + x) * 4) as usize;
        (out[i], out[i + 3])
    };
    let pad = (ow - w) / 2;
    assert_eq!(at(0, 0).1, 0, "左上角在投影之外,该全透明");
    assert_eq!(
        at(ow / 2, oh / 2),
        (128, 255),
        "卡心不许动"
    );
    // 卡的四角被圆角抠掉,只剩底下那层投影;不是全透明,但远不到卡面那档。
    assert!(
        at(pad, pad).1 < 128,
        "卡角没被圆角抠掉:alpha = {}",
        at(pad, pad).1
    );
    let (edge, ea) = at(ow / 2, pad);
    assert_eq!(ea, 255, "上边中点在卡面内");
    assert!(edge > 160, "上边中点该被描边提亮:{edge}");
    // 卡下方的余量里要有投影,而不是全透明。
    let below = at(ow / 2, oh - pad / 2).1;
    assert!(below > 0, "卡底下该有投影");
}

/// 空白卡走同一条烘焙:纯白卡面,颜色留给占位底色去乘。
#[test]
fn blank_card_is_a_white_face() {
    let (out, ow, oh) = bake_blank(48);
    let i = (((oh / 2) * ow + ow / 2) * 4) as usize;
    assert_eq!((out[i], out[i + 3]), (255, 255));
}
