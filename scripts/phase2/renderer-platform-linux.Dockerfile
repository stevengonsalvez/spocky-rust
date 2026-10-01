# Derived image for the Linux renderer qualification. Built once with network
# access, then run with --network none. The base is the pinned rust:1.94-bookworm
# digest used by the other Linux runners.
FROM rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq \
 && apt-get install -y -qq pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev libxdo-dev librsvg2-dev xvfb xauth x11-utils \
    xdotool imagemagick at-spi2-core python3-pyatspi python3-pil dbus-x11 \
    locales fonts-dejavu-core \
 && sed -i "s/^# *en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/" /etc/locale.gen \
 && locale-gen en_US.UTF-8 \
 && rm -rf /var/lib/apt/lists/*

# Fetch and build every locked dependency now so a run needs no network.
ENV CARGO_HOME=/opt/cargo-home \
    CARGO_TARGET_DIR=/opt/target \
    CARGO_BUILD_JOBS=2
WORKDIR /workspace
# rust-toolchain.toml pins 1.94.0 while the base ships 1.94.1; copying it makes
# this build install 1.94.0 now instead of rustup downloading it on every run.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked -p spocky-ui-renderer-pilot --bin spocky-ui-desktop \
    --no-default-features --features desktop
