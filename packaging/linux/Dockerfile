# Builds the Linux binary without a Linux host.
#
#   docker build -f packaging/linux/Dockerfile -t monolith-linux .
#   docker create --name monolith-build monolith-linux
#   docker cp monolith-build:/out/monolith-desktop dist/
#   docker rm monolith-build
#
# On a machine that already has a Rust toolchain, skip all of this and just run
# `cargo build --release` inside the Ubuntu VM. See packaging/linux/README.md.
FROM rust:1-bookworm

# The build needs the system libraries the crates link against: ALSA for audio,
# X/Wayland for winit. File dialogs use the desktop portal. Without these
# pkg-config fails deep inside a dependency with an unhelpful error.
RUN apt-get update && apt-get install --no-install-recommends -y \
        build-essential \
        pkg-config \
        libasound2-dev \
        libxkbcommon-dev \
        libwayland-dev \
        libx11-dev \
        libxrandr-dev \
        libxi-dev \
        libxcursor-dev \
        libgl1-mesa-dev \
        ca-certificates \
        dpkg-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

# Copy only build inputs; personal notes and Windows build caches stay outside.
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY ui ./ui
COPY assets ./assets
COPY examples ./examples
COPY installer/Cargo.toml ./installer/Cargo.toml
COPY packaging ./packaging
COPY LICENSE ./LICENSE
RUN cargo build --release --locked --bin monolith-desktop

# The tests need the dev dependencies and the examples, so they run against a
# fully built tree rather than the bin-only one above.
RUN cargo test --release --locked

RUN sh packaging/linux/build-deb.sh target/release/monolith-desktop /out

RUN mkdir -p /out \
    && cp target/release/monolith-desktop /out/ \
    && cp packaging/linux/monolith-desktop.desktop /out/ \
    && cp packaging/linux/monolith.desktop.metainfo.xml /out/ \
    && cp assets/monolith.svg /out/monolith-desktop.svg \
    && cp packaging/linux/install.sh /out/install.sh \
    && chmod +x /out/install.sh

CMD ["sh", "-c", "ls -la /out && file /out/monolith-desktop 2>/dev/null || true"]
