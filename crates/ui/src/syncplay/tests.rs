use similar_asserts::assert_eq;

use super::*;

/// 信令地址跟着 API 地址走,协议对应升级。
#[test]
fn signalling_url_follows_the_api_base() {
    assert_eq!(
        signalling_url("http://127.0.0.1:3000"),
        "ws://127.0.0.1:3000"
    );
    assert_eq!(
        signalling_url("https://example.com"),
        "wss://example.com"
    );
}

/// 边界:地址里没写协议时也得能推出一个能连的 ws 地址。
#[test]
fn signalling_url_handles_a_bare_host() {
    assert_eq!(
        signalling_url("127.0.0.1:3000"),
        "ws://127.0.0.1:3000"
    );
}

/// **同一台机器上的两个实例必须是两台设备。**
///
/// id 撞了的话服务端会按 id 入册,后连上的顶掉先连上的 —— 而现象是
/// 「另一台设备时有时无」,离病因极远。
#[test]
fn device_identity_distinguishes_two_instances() {
    let first = identity_from("nixos", 1234);
    let second = identity_from("nixos", 5678);

    assert_ne!(first.id, second.id);
    assert_ne!(
        first.name, second.name,
        "名字也得能区分,否则界面上两行长得一样"
    );
}

fn roster_of(
    devices: Vec<DeviceDto>,
) -> Arc<Mutex<Roster>> {
    let roster =
        Arc::new(Mutex::new(Roster::new("me".to_owned())));
    lock(&roster).update(devices);
    roster
}

/// 状态行上写的是设备名,不是信令里那个 id。
///
/// 用户在列表上点的是名字,状态行换个写法就会让人以为推给了别的设备。
#[test]
fn listening_line_uses_the_device_name() {
    let roster = roster_of(vec![DeviceDto {
        id: "pc1-42".to_owned(),
        name: "pc1 #42".to_owned(),
    }]);

    assert_eq!(display_name(&roster, "pc1-42"), "pc1 #42");
}

/// 边界:名册还没到就退回 id —— 一行 id 也好过一行空白。
#[test]
fn listening_line_falls_back_to_the_id() {
    let roster = roster_of(Vec::new());

    assert_eq!(display_name(&roster, "pc1-42"), "pc1-42");
}

/// 三个角色都要有人能读的文案。
#[test]
fn describe_role_covers_every_role() {
    for role in [
        Role::Alone,
        Role::Host {
            listeners: vec!["a".to_owned()],
        },
        Role::Listener {
            host: "a".to_owned(),
        },
    ] {
        assert!(
            !describe_role(&role).is_empty(),
            "{role:?} 没有文案"
        );
    }
}

/// 同播文案里的中文必须在子集字体里 —— 与 `music.rs` 那条同一个守卫。
///
/// 覆盖得到的只有**本层写死的那部分**:角色文案,加上 `syncplay` 三种错误的
/// 真实 `Display` 输出(不是手抄的,改了措辞而没重裁字体这里就红)。
///
/// 覆盖不到的是变量部分 —— 设备名由对端自报,服务端的错误说明里也带着它。
/// 那和歌名是同一类东西:任意文本,不可能预裁,桌面上落到系统字体。
#[test]
fn sync_copy_only_uses_subset_glyphs() {
    use syncplay::SyncError;

    const CJK_SUBSET: &[u8] =
        include_bytes!("../../fonts/cjk-subset.ttf");

    let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
        .expect("子集字体应能被解析");

    let mut copy: Vec<String> = [
        Role::Alone,
        Role::Host {
            listeners: vec!["a".to_owned()],
        },
        Role::Listener {
            host: "a".to_owned(),
        },
    ]
    .iter()
    .map(describe_role)
    .collect();

    // 失败那一行:前缀是本模块写死的,后半截取自三种错误的真实输出。
    for error in [
        SyncError::Signalling("timed out".to_owned()),
        SyncError::Peer("no candidates".to_owned()),
        SyncError::Envelope("expected value".to_owned()),
    ] {
        copy.push(format!("同播失败: {error}"));
    }

    for line in copy {
        for ch in line.chars() {
            assert!(
                face.glyph_index(ch).is_some(),
                "子集字体缺字 {ch:?}(同播文案 {line:?})—— 重跑 `just font-subset`"
            );
        }
    }
}
