//! `WallScene` 的 ECS 侧单测。
//!
//! 这三个被测方法拿的是 `&mut App`,但它们只增删实体、改组件、改资源,
//! 一行都不碰渲染子世界。所以测试不走 `Scene::new` 那条要真 wgpu adapter
//! 的路,自己搭一个只有 `MinimalPlugins` + `AssetPlugin` 的无头 App,
//! 把四种资产按需 `init_asset` 出来即可(见 [`headless_app`])。

use similar_asserts::assert_eq;

use super::*;

/// 浮点比较的容差。位姿都是像素量级,1e-4 足够区分「算错」与「浮点抖动」。
const EPS: f32 = 1e-4;

/// 搭一个够 `WallScene` 跑的最小 App。
///
/// `MinimalPlugins` 给出 App/时间/任务池,`AssetPlugin` 给出资产系统本身;
/// 四种资产是 `WallScene::new` / `sync_cards` / `apply_cover` 分别要的
/// `Assets<T>` 资源。这里刻意**不加** `RenderPlugin` 与 `MaterialPlugin`:
/// 前者要真 wgpu adapter,后者只在渲染时才需要,而 `MeshMaterial3d<T>`
/// 不过是个组件包装,插拔它不经过任何管线。
fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(bevy::asset::AssetPlugin::default())
        .init_asset::<Image>()
        .init_asset::<Mesh>()
        .init_asset::<StandardMaterial>()
        .init_asset::<FoilMaterial>();
    app
}

/// 一张位姿全给定的卡。字段顺序与 `WallCard` 一致,读起来不用回头查。
fn card(
    x: f32,
    y: f32,
    z: f32,
    rot_y: f32,
    rot_x: f32,
) -> WallCard {
    WallCard {
        x,
        y,
        z,
        rot_y,
        rot_x,
        dim: 1.0,
        size: 100.0,
    }
}

/// 一帧只带卡、不改尺寸、不放闪卡的输入。
fn frame_of(cards: Vec<WallCard>) -> WallFrame {
    WallFrame { cards, ..default() }
}

/// 一张 1×1 的纯色卡面。像素内容无关紧要,「有没有接过去」才是被测的。
fn cover_at(slot: usize) -> WallCover {
    WallCover {
        slot,
        width: 1,
        height: 1,
        rgba: vec![255, 255, 255, 255],
        blank: false,
    }
}

/// 断言两个浮点在容差内相等,失败时说清是哪一项、差了多少。
fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < EPS,
        "{what}:期望 {expected},实得 {actual}"
    );
}

/// 某个实体此刻挂的是不是闪卡材质。
fn is_foil(app: &App, entity: Entity) -> bool {
    app.world()
        .entity(entity)
        .contains::<MeshMaterial3d<FoilMaterial>>()
}

/// 某个实体此刻挂的是不是普通卡材质。
fn is_plain(app: &App, entity: Entity) -> bool {
    app.world()
        .entity(entity)
        .contains::<MeshMaterial3d<StandardMaterial>>()
}

/// ui 的 y 轴向下、bevy 向上,`sync_cards` 里那个负号写掉了整面墙会上下颠倒,
/// 而编译器和渲染都不会吭一声 —— 只有肉眼在真机上能看出来。同一个坐标系翻转
/// 也管着 `rot_x`(ui 以「顶边向后」为正),漏掉它则每张卡的俯仰反向。
#[test]
fn card_poses_flip_the_ui_y_axis_into_bevy() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let frame =
        frame_of(vec![card(30.0, 40.0, -5.0, 0.3, 0.2)]);
    wall.apply(&mut app, &frame);

    let entity = wall.cards[0];
    let t = app
        .world()
        .get::<Transform>(entity)
        .expect("摆过位姿的卡必须有 Transform");

    assert_close(t.translation.x, 30.0, "x 不该翻号");
    assert_close(
        t.translation.y,
        -40.0,
        "y 必须翻号,否则整面墙上下颠倒",
    );
    assert_close(t.translation.z, -5.0, "z 不该翻号");

    let (yaw, pitch, roll) =
        t.rotation.to_euler(EulerRot::YXZ);
    assert_close(yaw, 0.3, "rot_y 不该翻号");
    assert_close(
        pitch,
        -0.2,
        "rot_x 必须翻号,否则卡片俯仰反向",
    );
    assert_close(roll, 0.0, "卡片不该有滚转");
    assert_close(
        t.scale.x,
        100.0,
        "卡边长要原样写进 scale",
    );
}

/// 实体池只增不减:卡变多要补实体,卡变少要**复用**已有的而不是重建。
/// 每帧重建实体在画面上看不出来,却会让实体号一直漂,闪卡记住的槽位、
/// 材质句柄全跟着失效。这里钉死「老实体还是那几个」。
#[test]
fn the_card_pool_grows_and_reuses_its_entities() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    wall.apply(
        &mut app,
        &frame_of(vec![
            card(0.0, 0.0, 0.0, 0.0, 0.0),
            card(1.0, 0.0, 0.0, 0.0, 0.0),
        ]),
    );
    let first_two = wall.cards.clone();
    assert_eq!(first_two.len(), 2, "两张卡该建出两个实体");

    wall.apply(
        &mut app,
        &frame_of(vec![
            card(0.0, 0.0, 0.0, 0.0, 0.0),
            card(1.0, 0.0, 0.0, 0.0, 0.0),
            card(2.0, 0.0, 0.0, 0.0, 0.0),
        ]),
    );

    assert_eq!(
        wall.cards.len(),
        3,
        "第三张卡该让池子长到 3"
    );
    assert_eq!(
        wall.cards[..2].to_vec(),
        first_two,
        "老实体不该被重建,池子只允许在尾部追加"
    );
    assert_eq!(
        wall.materials.len(),
        3,
        "材质、纹理、底色三个并行数组必须跟实体池同长"
    );
    assert_eq!(wall.textures.len(), 3, "纹理槽数量对不上");
    assert_eq!(wall.tints.len(), 3, "底色槽数量对不上");
}

/// 池子不缩,所以卡变少时多出来的实体必须被藏掉。漏了这一步,上一帧那几张
/// 卡会顶着旧位姿留在墙上 —— 换歌单时表现为「幽灵卡」。
#[test]
fn cards_beyond_this_frames_count_are_hidden() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    wall.apply(
        &mut app,
        &frame_of(vec![
            card(0.0, 0.0, 0.0, 0.0, 0.0),
            card(1.0, 0.0, 0.0, 0.0, 0.0),
            card(2.0, 0.0, 0.0, 0.0, 0.0),
        ]),
    );
    wall.apply(
        &mut app,
        &frame_of(vec![card(0.0, 0.0, 0.0, 0.0, 0.0)]),
    );

    let vis = |e: Entity| {
        *app.world()
            .get::<Visibility>(e)
            .expect("卡实体建出来就带 Visibility")
    };
    assert_eq!(
        vis(wall.cards[0]),
        Visibility::Visible,
        "这一帧还在的卡不该被藏"
    );
    for idx in 1..3 {
        assert_eq!(
            vis(wall.cards[idx]),
            Visibility::Hidden,
            "多出来的实体必须藏掉,否则墙上留下上一帧的幽灵卡"
        );
    }
}

/// 普通卡的亮度是「占位底色 × dim」。dim 没写进材质,深处的卡就不会随深度
/// 压暗,整面墙糊成一片平的。
#[test]
fn dim_multiplies_into_the_plain_card_tint() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let mut c = card(0.0, 0.0, 0.0, 0.0, 0.0);
    c.dim = 0.5;
    wall.apply(&mut app, &frame_of(vec![c]));

    let [r, g, b] = wall.tints[0];
    let mat = app
        .world()
        .resource::<Assets<StandardMaterial>>()
        .get(&wall.materials[0])
        .expect("卡材质必须还在资产库里")
        .base_color
        .to_linear();
    assert_close(mat.red, r * 0.5, "红通道没乘上 dim");
    assert_close(mat.green, g * 0.5, "绿通道没乘上 dim");
    assert_close(mat.blue, b * 0.5, "蓝通道没乘上 dim");
}

/// 闪卡挪槽时,旧槽必须换回普通材质。只加不减的话两张卡同时闪,而且一个实体
/// 同时挂两种 `MeshMaterial3d` 会被画两遍 —— 这是 `sync_foil` 存在的全部理由。
#[test]
fn moving_the_foil_restores_the_old_slot_material() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let cards = vec![
        card(0.0, 0.0, 0.0, 0.0, 0.0),
        card(1.0, 0.0, 0.0, 0.0, 0.0),
    ];
    let mut frame = frame_of(cards);
    frame.foil = Some(0);
    wall.apply(&mut app, &frame);
    assert!(
        is_foil(&app, wall.cards[0])
            && !is_plain(&app, wall.cards[0]),
        "第 0 格该只剩闪卡材质"
    );

    frame.foil = Some(1);
    wall.apply(&mut app, &frame);

    assert!(
        !is_foil(&app, wall.cards[0]),
        "闪卡挪走后,旧槽不该还留着闪卡材质:墙上会同时亮两张"
    );
    assert!(
        is_plain(&app, wall.cards[0]),
        "旧槽必须换回普通材质,否则那张卡一点材质都没有"
    );
    assert!(
        is_foil(&app, wall.cards[1])
            && !is_plain(&app, wall.cards[1]),
        "新槽该只剩闪卡材质,同时挂两种会被画两遍"
    );
    assert_eq!(
        wall.foil_slot,
        Some(1),
        "记录的闪卡槽位没跟上"
    );
}

/// 停止播放(`foil` 变回 `None`)时也要还原。不还原的话,墙上会永远留着
/// 一张闪着的卡,而它对应的歌早就停了。
#[test]
fn clearing_the_foil_puts_the_plain_material_back() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let mut frame =
        frame_of(vec![card(0.0, 0.0, 0.0, 0.0, 0.0)]);
    frame.foil = Some(0);
    wall.apply(&mut app, &frame);

    frame.foil = None;
    wall.apply(&mut app, &frame);

    assert_eq!(wall.foil_slot, None, "闪卡槽位该被清空");
    assert!(
        !is_foil(&app, wall.cards[0]),
        "停播后墙上不该还留着一张闪卡"
    );
    assert!(
        is_plain(&app, wall.cards[0]),
        "还原后必须挂回普通材质"
    );
}

/// 越界的闪卡下标要被 filter 掉。ui 侧的槽位与这一帧的卡数是两条独立的路,
/// 换歌单的那一帧完全可能对不上;不过滤就会拿越界下标去索引 `self.materials`
/// 或 `self.textures`,直接 panic 掉整个渲染循环。
#[test]
fn an_out_of_range_foil_slot_is_ignored() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let mut frame = frame_of(vec![
        card(0.0, 0.0, 0.0, 0.0, 0.0),
        card(1.0, 0.0, 0.0, 0.0, 0.0),
    ]);
    frame.foil = Some(7);
    wall.apply(&mut app, &frame);

    assert_eq!(
        wall.foil_slot, None,
        "超出卡数的槽位必须被当作没有闪卡"
    );
    for &entity in &wall.cards {
        assert!(
            !is_foil(&app, entity),
            "越界槽位不该把闪卡材质挂到任何一张卡上"
        );
    }
}

/// 实体池比这一帧的卡多时(卡变少的那一帧),槽位可能落在池内却在 `cards`
/// 之外。`want` 的两个条件缺任何一个,`sync_foil` 末尾 `frame.cards.get(slot)`
/// 之外的 `self.textures[slot]` 就会读到一个这一帧根本不存在的槽位。
#[test]
fn a_foil_slot_past_this_frames_cards_is_ignored() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    wall.apply(
        &mut app,
        &frame_of(vec![
            card(0.0, 0.0, 0.0, 0.0, 0.0),
            card(1.0, 0.0, 0.0, 0.0, 0.0),
            card(2.0, 0.0, 0.0, 0.0, 0.0),
        ]),
    );

    // 池子里有 3 个实体,这一帧只剩 1 张卡,闪卡却还指着第 2 格。
    let mut frame =
        frame_of(vec![card(0.0, 0.0, 0.0, 0.0, 0.0)]);
    frame.foil = Some(2);
    wall.apply(&mut app, &frame);

    assert_eq!(
        wall.foil_slot, None,
        "槽位在实体池内但超出本帧卡数,同样要当作没有闪卡"
    );
    assert!(
        !is_foil(&app, wall.cards[2]),
        "已经藏掉的实体不该被点亮成闪卡"
    );
}

/// 闪卡材质只有一份,换槽时必须把**那一格的卡面**接过去。漏了这一步,
/// 正在放的那张卡会显示上一首的封面 —— 或者在从没设过封面时显示默认纹理。
/// 时钟与 dim 也一并核:time 不推进则光泽定格,dim 不写则闪卡不随深度压暗。
#[test]
fn the_foil_material_takes_over_its_slots_cover_and_clock()
{
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let mut c = card(0.0, 0.0, 0.0, 0.0, 0.0);
    c.dim = 0.25;
    let mut frame = frame_of(vec![c]);
    frame.covers = vec![cover_at(0)];
    frame.foil = Some(0);
    wall.apply(&mut app, &frame);

    let expected_cover = wall.textures[0]
        .clone()
        .expect("这一帧刚灌过封面,槽位纹理不该为空");
    let mat = app
        .world()
        .resource::<Assets<FoilMaterial>>()
        .get(&wall.foil)
        .expect("闪卡材质必须还在资产库里")
        .clone();

    assert!(
        mat.cover == expected_cover,
        "闪卡没接过这一格的卡面,墙上那张会顶着别人的封面"
    );
    assert_close(
        mat.params.dim,
        0.25,
        "闪卡的 dim 没写进 uniform",
    );
    assert_close(
        mat.params.time,
        1.0 / 60.0,
        "第一帧的时钟应是 1/60 秒;不推进则光泽定格",
    );

    wall.apply(&mut app, &frame);
    let time = app
        .world()
        .resource::<Assets<FoilMaterial>>()
        .get(&wall.foil)
        .expect("闪卡材质必须还在资产库里")
        .params
        .time;
    assert_close(
        time,
        2.0 / 60.0,
        "第二帧时钟该往前走一帧",
    );
}

/// 尺寸没变就不该重建离屏纹理。每帧重建等于每帧丢一张 GPU 纹理再建一张,
/// 而且 `finish` 的按身份缓存会永远失效 —— 画面照旧,代价全在后台。
#[test]
fn apply_rebuilds_the_target_only_when_the_size_changes() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let mut frame =
        frame_of(vec![card(0.0, 0.0, 0.0, 0.0, 0.0)]);
    frame.width = 800;
    frame.height = 600;
    wall.apply(&mut app, &frame);
    let after_resize = wall.target.clone();
    assert_eq!(
        wall.size,
        (800, 600),
        "尺寸变了要记下新尺寸"
    );

    wall.apply(&mut app, &frame);
    assert!(
        wall.target == after_resize,
        "尺寸没变还重建纹理,等于每帧白丢一张 GPU 纹理"
    );

    frame.width = 400;
    wall.apply(&mut app, &frame);
    assert!(
        wall.target != after_resize,
        "尺寸变了必须换新纹理,否则画面停在旧分辨率上"
    );
    assert_eq!(wall.size, (400, 600), "新尺寸没记下来");
}

/// 宽或高为 0 的那一帧要整个跳过 resize。Slint 在布局出结果之前会送来 0 尺寸,
/// 拿它去建纹理是 wgpu 的硬错误。
#[test]
fn a_zero_sized_frame_never_touches_the_target() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);
    let original = wall.target.clone();

    let mut frame =
        frame_of(vec![card(0.0, 0.0, 0.0, 0.0, 0.0)]);
    frame.width = 0;
    frame.height = 600;
    wall.apply(&mut app, &frame);
    assert!(
        wall.target == original,
        "宽为 0 的帧不该重建纹理"
    );

    frame.width = 800;
    frame.height = 0;
    wall.apply(&mut app, &frame);
    assert!(
        wall.target == original,
        "高为 0 的帧不该重建纹理"
    );
    assert_eq!(
        wall.size,
        (4, 4),
        "空尺寸不该被记成当前尺寸"
    );
}

/// 相机永远在 x = 0(平移已折进每张卡的回绕位置),下移 8% 高做出透视原点
/// 42% 的效果,并与 ui 侧的 `wall::project` 严格同构。这三个数任何一个写错,
/// 画面与命中测试就各说各话:看着点在卡上,点下去却选中了旁边那张。
#[test]
fn the_camera_sits_at_the_projection_origin() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let mut frame =
        frame_of(vec![card(0.0, 0.0, 0.0, 0.0, 0.0)]);
    frame.width = 800;
    frame.height = 600;
    frame.cam = WallCamera {
        dolly: 200.0,
        perspective: 1200.0,
    };
    wall.apply(&mut app, &frame);

    let t = app
        .world()
        .get::<Transform>(wall.camera)
        .expect("相机建出来就带 Transform");
    assert_close(
        t.translation.x,
        0.0,
        "相机的 x 必须恒为 0",
    );
    assert_close(
        t.translation.y,
        -0.08 * 600.0,
        "相机要下移 8% 高,这是透视原点 42% 的来源",
    );
    assert_close(
        t.translation.z,
        1000.0,
        "镜头距离该是 perspective - dolly",
    );

    let Projection::Perspective(p) = app
        .world()
        .get::<Projection>(wall.camera)
        .expect("apply 必须写入投影")
    else {
        panic!(
            "卡墙相机必须是透视投影,正交会让整面墙失去景深"
        );
    };
    assert_close(
        p.fov,
        2.0 * (300.0f32 / 1200.0).atan(),
        "竖直视野要由目标高与 perspective 反推",
    );
}

/// dolly 推过 perspective 时距离要被夹到 2.0。不夹则镜头穿到 z=0 平面背后
/// (甚至负距离),画面整个翻过来。
#[test]
fn an_overshooting_dolly_is_clamped_in_front_of_the_wall() {
    let mut app = headless_app();
    let mut wall = WallScene::new(&mut app);

    let mut frame =
        frame_of(vec![card(0.0, 0.0, 0.0, 0.0, 0.0)]);
    frame.cam = WallCamera {
        dolly: 5000.0,
        perspective: 1200.0,
    };
    wall.apply(&mut app, &frame);

    let z = app
        .world()
        .get::<Transform>(wall.camera)
        .expect("相机建出来就带 Transform")
        .translation
        .z;
    assert_close(
        z,
        2.0,
        "推过头的镜头必须夹在墙前面,不能穿到背后",
    );
}
