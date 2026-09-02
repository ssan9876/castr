FROM debian:trixie
ENV DEBIAN_FRONTEND=noninteractive
RUN dpkg --add-architecture arm64 && apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake pkg-config curl ca-certificates git autoconf automake libtool \
    gcc-aarch64-linux-gnu g++-aarch64-linux-gnu \
    libasound2-dev:arm64 libudev-dev:arm64 libdbus-1-dev:arm64 libxkbcommon-dev:arm64 \
    libx11-dev:arm64 libxext-dev:arm64 libxrandr-dev:arm64 libxcursor-dev:arm64 \
    libxi-dev:arm64 libxss-dev:arm64 libdrm-dev:arm64 libgbm-dev:arm64 \
    libgles2-mesa-dev:arm64 libegl1-mesa-dev:arm64 libopus-dev:arm64 \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
    --default-toolchain stable -c clippy -c rustfmt -t aarch64-unknown-linux-gnu
ENV PATH=/root/.cargo/bin:$PATH \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_LIBDIR=/usr/lib/aarch64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    CMAKE_POLICY_VERSION_MINIMUM=3.5 \
    LIBOPUS_STATIC=1
WORKDIR /src
