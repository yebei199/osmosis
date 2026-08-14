use super::*;

fn wide() -> WallLayout {
    layout(1808.0, 864.0, false)
}

/// 布局全部按容器尺寸推导:同比放大容器,布局同比放大。
#[test]
fn layout_scales_with_container() {
    let a = layout(904.0, 432.0, false);
    let b = layout(1808.0, 864.0, false);
    assert!((b.card - a.card * 2.0).abs() < 0.01);
    assert!(
        (b.col_pitch - a.col_pitch * 2.0).abs() < 0.01
    );
    assert!(
        (b.row_pitch - a.row_pitch * 2.0).abs() < 0.01
    );
    // 参考尺寸下取回设计稿原值。
    assert!((a.card - 150.0).abs() < 0.01);
    assert!((a.col_pitch - 204.0).abs() < 0.01);
}

/// 紧凑版式卡更小、列距更密:420 宽下一屏能放下约两列。
#[test]
fn compact_layout_fits_two_columns() {
    let lay = layout(420.0, 700.0, true);
    assert!(lay.col_pitch * 2.0 < 420.0);
    assert!(lay.card < 90.0);
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
    for i in 0..WALL_MAX_CARDS {
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

/// 松手后惯性衰减并吸附到列距整数倍,最终必须停(省电门依据)。
#[test]
fn released_camera_snaps_and_freezes() {
    let lay = wide();
    let mut cam = WallCam::default();
    cam.drag(-90.0, 0.0);
    assert!(cam.step(lay.col_pitch));
    cam.release();
    for _ in 0..300 {
        cam.step(lay.col_pitch);
    }
    assert!(
        !cam.step(lay.col_pitch),
        "300 帧后还在动,吸附不收敛"
    );
    let ratio = cam.pan_x / lay.col_pitch;
    assert!(
        (ratio - ratio.round()).abs() < 0.01,
        "没吸到格点:pan_x = {}",
        cam.pan_x
    );
    assert_eq!(cam.yaw, 0.0);
}

/// 滚轮推拉有行程限制,推不穿墙。
#[test]
fn wheel_dolly_is_clamped() {
    let mut cam = WallCam::default();
    for _ in 0..100 {
        cam.wheel(200.0);
    }
    assert!(cam.dolly <= 620.0);
    for _ in 0..100 {
        cam.wheel(-200.0);
    }
    assert!(cam.dolly >= -260.0);
}

/// 命中测试:把某张卡投到屏幕,再拿投影中心去问,要拿回同一张。
#[test]
fn hit_test_finds_the_projected_card() {
    let lay = wide();
    let cam = WallCam::default();
    let poses: Vec<CardPose> = (0..12)
        .map(|i| card_pose(&lay, i, 1.0))
        .collect();
    let world = world_pose(&cam, &poses[4]);
    let (sx, sy, _) =
        project(&lay, &cam, &world).unwrap();
    let hit =
        hit_test(&lay, &cam, &poses, sx, sy).unwrap();
    // 命中的要么是它自己,要么是遮在它前面(z 更大)的卡。
    assert!(world_pose(&cam, &poses[hit]).z >= world.z);
}

/// 空白处点不中任何卡。
#[test]
fn hit_test_misses_empty_space() {
    let lay = wide();
    let cam = WallCam::default();
    let poses: Vec<CardPose> = (0..6)
        .map(|i| card_pose(&lay, i, 1.0))
        .collect();
    assert_eq!(
        hit_test(&lay, &cam, &poses, 1.0, 1.0),
        None
    );
}

/// 塌回与 dolly 都在有限帧内收敛(420ms ≈ 25 帧的量级)。
#[test]
fn transitions_settle_within_a_second() {
    let mut c = Collapse::default();
    c.target = 0.0;
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

/// 圆角烘焙:角上透明、中心不动、非角边缘不动。
#[test]
fn rounded_corners_are_baked_into_alpha() {
    let (w, h) = (32u32, 32u32);
    let mut rgba = vec![255u8; (w * h * 4) as usize];
    bake_rounded(&mut rgba, w, h);
    let alpha = |x: u32, y: u32| {
        rgba[((y * w + x) * 4 + 3) as usize]
    };
    assert_eq!(alpha(0, 0), 0, "左上角该抠掉");
    assert_eq!(alpha(31, 31), 0, "右下角该抠掉");
    assert_eq!(alpha(16, 16), 255, "中心不许动");
    assert_eq!(alpha(16, 0), 255, "上边中点不在角区");
}
