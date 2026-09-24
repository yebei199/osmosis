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

/// **冷启动后还是同一台设备,而同机两个实例在界面上仍分得开。**
///
/// 曾经的规矩是反的:id 带进程号,两个实例因此是两台设备(服务端按 id 入册,
/// 同 id 会互相顶掉)。#100 之后 id 落盘,换来的是遥控器重连认得出自己
/// (#95 的 `resume` 分支);两个本机实例要在服务端也分得开,给第二个指一份
/// 自己的 `OSMOSIS_DEVICE_FILE`。名字仍带进程号,列表里才不是两行一样的。
#[test]
fn a_persisted_device_id_outlives_the_process() {
    let first = identity_from(
        "nixos",
        1234,
        "nixos-1234".to_owned(),
    );
    let second = identity_from(
        "nixos",
        5678,
        "nixos-1234".to_owned(),
    );

    assert_eq!(
        first.id, second.id,
        "冷启动后该还是同一个 id"
    );
    assert_ne!(
        first.name, second.name,
        "名字也得能区分,否则界面上两行长得一样"
    );
}

/// 连接状态文案里的中文必须在子集字体里 —— 与 `music.rs` 那条同一个守卫。
///
/// 覆盖得到的只有**本层写死的那部分**:失败前缀配上 `syncplay` 错误的真实
/// `Display` 输出(不是手抄的,改了措辞而没重裁字体这里就红)。
///
/// 覆盖不到的是变量部分 —— 设备名由对端自报,服务端的错误说明里也带着它。
/// 那和歌名是同一类东西:任意文本,不可能预裁,桌面上落到系统字体。
#[test]
fn link_copy_only_uses_subset_glyphs() {
    use syncplay::SyncError;

    const CJK_SUBSET: &[u8] =
        include_bytes!("../../../fonts/cjk-subset.ttf");

    let face = ttf_parser::Face::parse(CJK_SUBSET, 0)
        .expect("子集字体应能被解析");

    let copy = [
        describe_link_failure(
            &SyncError::Signalling("timed out".to_owned())
                .to_string(),
        ),
        describe_link_failure(
            &SyncError::Unauthorized.to_string(),
        ),
    ];

    for line in copy {
        for ch in line.chars() {
            assert!(
                face.glyph_index(ch).is_some(),
                "子集字体缺字 {ch:?}(连接文案 {line:?})—— 重跑 `just font-subset`"
            );
        }
    }
}

/// 版本对不上那句话要把**两个**版本号都说出来。
///
/// 少了它,用户只知道"用不了",不知道该升哪一端 —— 而这正是这道协商
/// 与「等一等就好」的掉线唯一分得开的地方(#109 AC-7)。
#[test]
fn the_version_message_names_both_sides() {
    let told = describe_incompatible(3, Some(2));

    assert!(told.contains('3'), "少了本机的版本号: {told}");
    assert!(told.contains('2'), "少了对端的版本号: {told}");
    assert!(told.contains("升级"), "没说该做什么: {told}");
}

/// 旧到根本不报版本的对端:说「太旧」,不要编一个号出来。
///
/// 编一个(比如 0)的话,用户会拿着那个号去找一个不存在的版本。
#[test]
fn a_server_too_old_to_report_is_said_so() {
    let told = describe_incompatible(3, None);

    assert!(told.contains("太旧"), "{told}");
    assert!(
        !told.contains('0'),
        "对端报不出版本时不该编一个号: {told}"
    );
}

/// 「版本不对」与「普通掉线」不是同一句话。
///
/// 同一句的话,一个等多久都不会好的状态会被说成一个等一等就好的状态。
/// 落点也不同(横幅 vs 几秒就消失的提示),那一半在 `handle` 的两条分支里。
#[test]
fn a_version_clash_does_not_read_like_a_disconnect() {
    let clash = describe_incompatible(3, Some(2));
    let dropped = describe_link_failure("连接已关闭");

    assert_ne!(clash, dropped);
    assert!(
        !dropped.contains("升级"),
        "普通掉线不该叫人去升级: {dropped}"
    );
}
