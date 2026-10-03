# Derived image for the Linux renderer qualification. Built once with network
# access, then run with --network none. The base is the pinned rust:1.94-bookworm
# digest used by the other Linux runners.
FROM rust@sha256:6ae102bdbf528294bc79ad6e1fae682f6f7c2a6e6621506ba959f9685b308a55

# The runner passes the SHA-256 of the Cargo.lock this image was built from and
# refuses to run against a different lock, because offline resolution needs every
# workspace member's locked packages in the image.
ARG CARGO_LOCK_SHA256
LABEL org.spocky.cargo-lock-sha256=$CARGO_LOCK_SHA256

# Debian snapshot at a fixed time, so the same package versions install later.
ENV DEBIAN_FRONTEND=noninteractive
RUN printf '%s\n' \
    'Types: deb' \
    'URIs: http://snapshot.debian.org/archive/debian/20261001T170000Z/' \
    'Suites: bookworm bookworm-updates' \
    'Components: main' \
    'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
    '' \
    'Types: deb' \
    'URIs: http://snapshot.debian.org/archive/debian-security/20261001T170000Z/' \
    'Suites: bookworm-security' \
    'Components: main' \
    'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' \
    > /etc/apt/sources.list.d/debian.sources \
 && apt-get -o Acquire::Check-Valid-Until=false update -qq \
 && apt-get install -y -qq \
    pkg-config=1.8.1-1 \
    libgtk-3-dev=3.24.38-2~deb12u3 \
    libwebkit2gtk-4.1-dev=2.50.6-1~deb12u2 \
    libayatana-appindicator3-dev=0.5.92-1 \
    libxdo-dev=1:3.20160805.1-5 \
    librsvg2-dev=2.54.7+dfsg-1~deb12u1 \
    xvfb=2:21.1.7-3+deb12u13 \
    xauth=1:1.1.2-1 \
    x11-utils=7.7+5 \
    xdotool=1:3.20160805.1-5 \
    imagemagick=8:6.9.11.60+dfsg-1.6+deb12u13 \
    at-spi2-core=2.46.0-5 \
    python3-pyatspi=2.46.0-2 \
    python3-pil=9.4.0-1.1+deb12u1 \
    dbus-x11=1.14.10-1~deb12u1 \
    locales=2.36-9+deb12u14 \
    fonts-dejavu-core=2.37-6 \
 && sed -i "s/^# *en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/" /etc/locale.gen \
 && locale-gen en_US.UTF-8 \
 && rm -rf /var/lib/apt/lists/*

# Fetch every locked package of the whole workspace, then build the pilot, so a
# run needs no network. rust-toolchain.toml pins 1.94.0 while the base ships
# 1.94.1; copying it makes this build install 1.94.0 now instead of rustup
# downloading it on every run.
ENV CARGO_HOME=/opt/cargo-home \
    CARGO_TARGET_DIR=/opt/target \
    CARGO_BUILD_JOBS=2
WORKDIR /workspace
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo fetch --locked \
 && cargo build --locked -p spocky-ui-renderer-pilot --bin spocky-ui-desktop \
    --no-default-features --features desktop
