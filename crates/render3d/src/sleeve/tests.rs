use super::*;

/// 几个有代表性的位姿:原位、交接中途的两张、拖到上限。
fn poses() -> [Transform; 4] {
    [
        Transform::default(),
        pose(motion::incoming(0.3), 0.2, -0.4),
        pose(motion::outgoing(0.7), -0.1, 0.5),
        pose(
            motion::Offset::default(),
            motion::MAX_PITCH,
            motion::MAX_YAW,
        ),
    ]
}

/// 倒影实体(镜像网格 + [`mirror`] 的位姿)上的每个顶点,正好是封套对应顶点
/// 关于地面的镜像 —— 四元数那一步写错一个符号,倒影就会朝另一边转。
#[test]
fn the_reflection_is_the_slab_mirrored_in_the_floor() {
    let slab = mesh::slab();
    let mirrored = mesh::mirrored(mesh::slab());
    for pose in poses() {
        let reflected = mirror(&pose);
        for (p, q) in
            slab.positions.iter().zip(&mirrored.positions)
        {
            let world =
                pose.transform_point(Vec3::from_array(*p));
            let expected = Vec3::new(
                world.x,
                2.0 * FLOOR_Y - world.y,
                world.z,
            );
            let got = reflected
                .transform_point(Vec3::from_array(*q));
            assert!(
                (got - expected).length() < 1e-4,
                "{pose:?}:顶点 {p:?} 的倒影在 {got},该在 {expected}"
            );
        }
    }
}

/// 呼吸与拖动的整个范围里,封套都不扎进地面:扎进去的那一截在倒影那侧被剪掉,
/// 封套这侧却照画,看着就是插在地里。
#[test]
fn the_slab_stays_above_the_floor_while_tilting() {
    let slab = mesh::slab();
    for pitch in
        [-motion::MAX_PITCH, 0.0, motion::MAX_PITCH]
    {
        for yaw in [-motion::MAX_YAW, 0.0, motion::MAX_YAW]
        {
            let pose =
                pose(motion::Offset::default(), pitch, yaw);
            let lowest = slab
                .positions
                .iter()
                .map(|p| {
                    pose.transform_point(Vec3::from_array(
                        *p,
                    ))
                    .y
                })
                .fold(f32::MAX, f32::min);
            assert!(
                lowest > FLOOR_Y,
                "pitch {pitch}、yaw {yaw} 时最低点 {lowest} 低于地面 {FLOOR_Y}"
            );
        }
    }
}

/// 半透明只给倒影与地面,封套不透明;安卓走抖动。
#[test]
fn only_the_reflection_and_floor_are_translucent() {
    let solid = SleeveMaterial {
        params: SleeveParams::default(),
        cover: Handle::default(),
        translucent: false,
    };
    assert!(matches!(
        solid.alpha_mode(),
        AlphaMode::Opaque
    ));
    let glass = SleeveMaterial {
        translucent: true,
        ..solid
    };
    assert!(!matches!(
        glass.alpha_mode(),
        AlphaMode::Opaque
    ));
    assert_eq!(
        DITHER,
        u32::from(cfg!(target_os = "android"))
    );
}
