//! 从 bang-dream 的 `.proto` 生成 gRPC 客户端。
//!
//! `.proto` 来自 `third_party/bang-dream` 这个 submodule —— 与 Go 侧是同一份文件,
//! 两端都由 protoc 生成,没有任何一边手写(bang-dream 的 `docs/adr/0001`)。
//!
//! 只生成 client:本服务是 bang-dream 的调用方,不实现它的 service。

use std::path::PathBuf;

fn main() {
    // 相对 CARGO_MANIFEST_DIR 定位,不写死绝对路径。
    let repo_root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("server/ 必然有父目录")
            .to_path_buf();
    let proto_dir =
        repo_root.join("third_party/bang-dream/proto");
    let proto = proto_dir.join("music/v1/music.proto");

    assert!(
        proto.exists(),
        "找不到 {} —— submodule 没拉?试 `git submodule update --init`",
        proto.display()
    );

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&[&proto], &[&proto_dir])
        .expect("protoc 生成失败");

    println!("cargo:rerun-if-changed={}", proto.display());
}
