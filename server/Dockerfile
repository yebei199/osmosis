# osmosis 后端(axum)的服务端镜像。
#
# 从仓库根构建,不是从 server/ 构建 —— 它依赖 workspace 里的 contract crate:
#   docker build -f server/Dockerfile -t ghcr.io/yebei199/osmosis-server:<tag> .
# 国内网络下 cargo 拉 crates.io 需要代理:
#   docker build --network=host --build-arg HTTPS_PROXY=$HTTPS_PROXY ...
#
# 构建期不需要数据库:server 里全是运行时 sqlx::query(),没有编译期检查的宏,
# 所以不需要 .sqlx 离线目录。迁移由进程自己在启动时跑(server/src/db.rs)。
#
# 运行期环境变量:BIND、BANG_DREAM_ADDR、DATABASE_URL、INVITE_CODE。

FROM rust:1-bookworm AS builder
# protoc 生成 bang-dream 的 gRPC 客户端(server/build.rs);clang 是 .cargo/config.toml
# 指定的链接器,不装它整棵依赖树的 build script 都链不起来。
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler clang \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
# 只编 server 一个包:workspace 里还有 Slint/Bevy 客户端,它们要 GPU 和 GUI 依赖,
# 与本镜像无关。
RUN cargo build --release -p server --locked
RUN strip /src/target/release/server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -u 10001 -m osmosis
COPY --from=builder /src/target/release/server /usr/local/bin/osmosis-server
USER osmosis
# 默认绑 0.0.0.0:容器里绑回环等于谁也连不上。
ENV BIND=0.0.0.0:3000
ENTRYPOINT ["/usr/local/bin/osmosis-server"]
