# osmosis 后端(axum)的服务端镜像。
#
# **目标平台是 arm64**:唯一的部署目标 main-vps 是 aarch64 机器。开发机是 x86_64,
# 所以这里交叉编译。QEMU 模拟下编一遍这棵依赖树要几十分钟,交叉编译是几分钟,
# 代价只是多装一个 aarch64 的 gcc 当链接器。运行阶段刻意不含任何 RUN:在 arm64
# 阶段执行命令就需要 binfmt/QEMU,而开发机上没有注册。
#
# 从仓库根构建,不是从 server/ 构建 —— 它依赖 workspace 里的 contract crate:
#   docker buildx build --platform linux/arm64 -f server/Dockerfile \
#     -t ghcr.io/yebei199/osmosis-server:<tag> .
# 国内网络下 cargo 拉 crates.io 需要代理:
#   docker buildx build --network=host --build-arg HTTPS_PROXY=$HTTPS_PROXY ...
#
# 构建期不需要数据库:server 里全是运行时 sqlx::query(),没有编译期检查的宏,
# 所以不需要 .sqlx 离线目录。迁移由进程自己在启动时跑(server/src/db.rs)。
#
# 运行期环境变量:BIND、BANG_DREAM_ADDR、DATABASE_URL、INVITE_CODE。

FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder
# protoc 生成 bang-dream 的 gRPC 客户端(server/build.rs);gcc-aarch64-linux-gnu 既是
# 交叉链接器,也是 cc crate 编译 C 依赖(rustls 的 ring/aws-lc)时用的编译器。
# clang 仍然要装:build script 是给**宿主**目标 x86_64 编的,而 .cargo/config.toml
# 给那个目标指定了 clang 作链接器。少了它,serde/libc 这些的 build script 先炸。
# libc6-dev-arm64-cross 提供 aarch64 的 glibc 启动文件(Scrt1.o/crti.o):
# gcc-aarch64-linux-gnu 只有编译器,没有目标平台的 libc,少了它最后一步链接才报错。
RUN apt-get update && apt-get install -y --no-install-recommends \
        protobuf-compiler clang gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
    && rm -rf /var/lib/apt/lists/*
# 注意 .cargo/config.toml 只给 x86_64-unknown-linux-gnu 指定了 clang 链接器,
# 交叉目标不受它影响,这里给的是 aarch64 自己那一份。
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
WORKDIR /src
COPY . .
# 必须在 COPY 之后:rust-toolchain.toml 会把 rustup 切到它指定的 stable 工具链,
# 那是与镜像自带的默认工具链不同的一份。装在 COPY 之前等于装给了没人用的那份,
# 报错是 E0463 找不到 core,离"少了个 target"很远。
RUN rustup target add aarch64-unknown-linux-gnu
# 只编 server 一个包:workspace 里还有 Slint/Bevy 客户端,它们要 GPU 和 GUI 依赖,
# 与本镜像无关。
RUN cargo build --release -p server --locked --target aarch64-unknown-linux-gnu \
    && aarch64-linux-gnu-strip /src/target/aarch64-unknown-linux-gnu/release/server

FROM debian:bookworm-slim
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /src/target/aarch64-unknown-linux-gnu/release/server /usr/local/bin/osmosis-server
USER 10001:10001
# 默认绑 0.0.0.0:容器里绑回环等于谁也连不上。
ENV BIND=0.0.0.0:3000
ENTRYPOINT ["/usr/local/bin/osmosis-server"]
