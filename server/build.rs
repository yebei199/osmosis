//! 从本仓库存的 `.proto` 生成 gRPC 客户端。
//!
//! 契约的上游是 bang-dream —— 与 Go 侧同一份文件,两端都由 protoc 生成,没有任何一边
//! 手写(bang-dream 的 `docs/adr/0001`)。这里存的是它的副本,而不是直接读
//! `third_party/bang-dream` 那个 submodule:上游是私有仓库,为一个 11KB 的自包含文件
//! 要求 CI 持有跨仓库凭据,不成比例。漂移由 `cargo xtask boundaries` 在 submodule
//! 在场时挡住。
//!
//! 只生成 client:本服务是 bang-dream 的调用方,不实现它的 service。

use std::path::PathBuf;

fn main() {
    // 相对 CARGO_MANIFEST_DIR 定位,不写死绝对路径。
    let proto_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("proto");
    let proto = proto_dir.join("music/v1/music.proto");

    assert!(
        proto.exists(),
        "找不到 {} —— 它是仓库内容,不是 submodule,不该缺失",
        proto.display()
    );

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&[&proto], &[&proto_dir])
        .expect("protoc 生成失败");

    println!("cargo:rerun-if-changed={}", proto.display());
}
