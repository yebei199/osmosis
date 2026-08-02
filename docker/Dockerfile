# Osmosis Android APK 的构建环境。
#
# 包含: JDK 17、Android SDK(platform 34 + build-tools)、NDK r27、
# Gradle、带 Android target 的 stable Rust,以及 cargo-ndk。
#
# 构建镜像:      docker build -t osmosis-builder docker/
# 构建 APK:      见 ./build.sh(把仓库挂载到 /work,并在容器内
#                运行 cargo xtask android)

ARG BASE_IMAGE=ubuntu:24.04
FROM ${BASE_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl unzip zip git \
        openjdk-17-jdk-headless \
        build-essential pkg-config \
        libfontconfig1-dev \
    && rm -rf /var/lib/apt/lists/*

# ---- Android SDK + NDK ------------------------------------------------------
# JVM 代理参数(宿主机 build.sh 在设置了代理时会传入
# -Dhttp(s).proxyHost=...),让 sdkmanager 能在代理后访问
# dl.google.com。默认为空;curl/rustup/cargo 会读取预置的
# http_proxy 构建参数。
ARG JAVA_TOOL_OPTIONS=
ENV ANDROID_HOME=/opt/android-sdk
ENV ANDROID_SDK_ROOT=$ANDROID_HOME
ARG CMDLINE_TOOLS=11076708
ARG ANDROID_PLATFORM=34
ARG BUILD_TOOLS=34.0.0
ARG NDK_VERSION=27.2.12479018

RUN mkdir -p $ANDROID_HOME/cmdline-tools \
    && curl -fsSL -o /tmp/clt.zip \
        "https://dl.google.com/android/repository/commandlinetools-linux-${CMDLINE_TOOLS}_latest.zip" \
    && unzip -q /tmp/clt.zip -d $ANDROID_HOME/cmdline-tools \
    && mv $ANDROID_HOME/cmdline-tools/cmdline-tools $ANDROID_HOME/cmdline-tools/latest \
    && rm /tmp/clt.zip

ENV PATH=$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH

RUN export JAVA_TOOL_OPTIONS="$JAVA_TOOL_OPTIONS"; yes | sdkmanager --licenses > /dev/null \
    && sdkmanager \
        "platform-tools" \
        "platforms;android-${ANDROID_PLATFORM}" \
        "build-tools;${BUILD_TOOLS}" \
        "ndk;${NDK_VERSION}"

ENV ANDROID_NDK_HOME=$ANDROID_HOME/ndk/${NDK_VERSION}
ENV ANDROID_NDK_ROOT=$ANDROID_NDK_HOME

# ---- Gradle -----------------------------------------------------------------
ARG GRADLE_VERSION=8.11.1
RUN curl -fsSL -o /tmp/gradle.zip \
        "https://services.gradle.org/distributions/gradle-${GRADLE_VERSION}-bin.zip" \
    && unzip -q /tmp/gradle.zip -d /opt \
    && rm /tmp/gradle.zip \
    && ln -s /opt/gradle-${GRADLE_VERSION}/bin/gradle /usr/local/bin/gradle

# ---- Rust -------------------------------------------------------------------
ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && rustup target add \
        aarch64-linux-android \
        armv7-linux-androideabi \
        x86_64-linux-android

ARG CARGO_NDK_VERSION=4.1.2
RUN cargo install cargo-ndk --version ${CARGO_NDK_VERSION} --locked

WORKDIR /work
CMD ["/bin/bash"]
