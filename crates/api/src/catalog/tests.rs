use super::*;

fn dto(protocol_version: u32) -> HealthDto {
    HealthDto {
        status: "ok".to_owned(),
        protocol_version,
    }
}

/// 版本一致时原样放行,且不吞掉 dto 的其他字段。
#[test]
fn matching_version_returns_dto() {
    let ok = check_version(dto(PROTOCOL_VERSION))
        .expect("版本一致时不应报错");
    assert_eq!(ok.protocol_version, PROTOCOL_VERSION);
    assert_eq!(ok.status, "ok");
}

/// 客户端旧、服务端新:必须报错而不是接受。
#[test]
fn newer_server_version_is_mismatch() {
    let err = check_version(dto(PROTOCOL_VERSION + 1))
        .expect_err("版本更高时应报错");
    assert!(matches!(
        err,
        ApiError::VersionMismatch { expected, actual }
            if expected == PROTOCOL_VERSION
                && actual == PROTOCOL_VERSION + 1
    ));
}

/// 客户端新、服务端旧:不匹配是对称的,不存在"向后兼容就放行"。
#[test]
fn older_server_version_is_mismatch() {
    let older = PROTOCOL_VERSION - 1;
    let err = check_version(dto(older))
        .expect_err("版本更低时应报错");
    assert!(matches!(
        err,
        ApiError::VersionMismatch { expected, actual }
            if expected == PROTOCOL_VERSION && actual == older
    ));
}

/// 边界:服务端给了 0 —— 正是 `health` 注释里说的"字段静默变成默认值"的场景,
/// 必须被拦下而不是当成合法版本。
///
/// 注意:当前实现下它与上一个用例走同一条 `!=` 分支,并非靠红转绿证明。
/// 留着它是为了钉住意图:将来若有人把校验改成"仅当 actual > expected 才报错",
/// 这里会红。
#[test]
fn zero_server_version_is_mismatch() {
    let err = check_version(dto(0))
        .expect_err("版本为 0 时应报错");
    assert!(matches!(
        err,
        ApiError::VersionMismatch { actual: 0, .. }
    ));
}
