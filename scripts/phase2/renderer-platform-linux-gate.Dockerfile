# Linux gate image: the shipped Paseo desktop app built as the reference ships it,
# the pinned Electron 44.2.0 host (host B), the pinned CEF host (host A), and the
# Dioxus web bundle. Built once with network access, run with --network none.
FROM node@sha256:e929171d35b9df7773a3ec5b068e387fa109441dc90f91e6560af5d39b7e9bf1

ARG CEF_ARCHIVE
ARG CEF_SHA256
ARG REFERENCE_COMMIT
ARG BUNDLE_SHA256
LABEL org.spocky.reference-commit=$REFERENCE_COMMIT \
      org.spocky.cef-sha256=$CEF_SHA256 \
      org.spocky.bundle-sha256=$BUNDLE_SHA256

# Debian snapshot at a fixed time, every named package at an exact version.
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
 && apt-get install -y -qq --allow-downgrades \
    xvfb=2:21.1.7-3+deb12u13 \
    xauth=1:1.1.2-1 \
    x11-utils=7.7+5 \
    libgtk-3-0=3.24.38-2~deb12u3 \
    libnss3=2:3.87.1-1+deb12u4 \
    libxss1=1:1.2.3-1 \
    libasound2=1.2.8-1+b1 \
    libgbm1=22.3.6-1+deb12u2 \
    libxtst6=2:1.2.3-1.1 \
    libatspi2.0-0=2.46.0-5 \
    libdrm2=2.4.114-1+b1 \
    libcups2=2.4.2-3+deb12u9 \
    libxkbcommon0=1.5.0-1 \
    fonts-dejavu-core=2.37-6 \
    locales=2.36-9+deb12u14 \
    dbus-x11=1.14.10-1~deb12u1 \
    ca-certificates=20250419~deb12u1 \
    curl=7.88.1-10+deb12u15 \
    bzip2=1.0.8-5+b1 \
    build-essential=12.9 \
    cmake=3.25.1-1 \
    ninja-build=1.11.1-2~deb12u1 \
    libx11-dev=2:1.8.4-2+deb12u2 \
    python3-pil=9.4.0-1.1+deb12u1 \
 && sed -i "s/^# *en_US.UTF-8 UTF-8/en_US.UTF-8 UTF-8/" /etc/locale.gen \
 && locale-gen en_US.UTF-8 \
 && rm -rf /var/lib/apt/lists/*

# The shipped app, built as packages/desktop ships it: app deps, the production web
# export with PASEO_WEB_PLATFORM=electron, then the desktop build restricted to an
# unpacked, unsigned directory.
COPY reference /ref
WORKDIR /ref
ENV CI=1 NODE_OPTIONS=--max-old-space-size=3072
ENV PATH=/ref/node_modules/.bin:$PATH
RUN npm ci --ignore-scripts --no-audit --no-fund \
 && node scripts/postinstall-patches.mjs
RUN npm run build:app-deps:clean
RUN cd packages/app && PASEO_WEB_PLATFORM=electron npx expo export --platform web
RUN npm run build --workspace=@getpaseo/desktop -- --dir --publish never

# Host B: the pinned Electron 44.2.0 from the committed lockfile.
COPY hostb /hostb
# The installer downloads the binary and checks it against the release SHASUMS256.
RUN cd /hostb && npm ci --no-audit --no-fund \
 && node node_modules/electron/install.js \
 && node -p "require('electron')"

# Host A: the pinned CEF binary distribution, verified by SHA-256, and the host.
RUN mkdir /cef && cd /cef \
 && curl -fsSL -o cef.tar.bz2 "https://cef-builds.spotifycdn.com/$(printf '%s' "$CEF_ARCHIVE" | sed 's/+/%2B/g')" \
 && echo "$CEF_SHA256  cef.tar.bz2" | sha256sum -c - \
 && tar xjf cef.tar.bz2 && rm cef.tar.bz2
COPY cefhost /cefhost
RUN cmake -S /cefhost -B /cefbuild -G Ninja -DCEF_ROOT="$(ls -d /cef/cef_binary_*)" \
      -DCMAKE_BUILD_TYPE=Release -DPROJECT_ARCH=x86_64 \
 && ninja -C /cefbuild spocky-cef-host

# Last, so a new bundle does not rebuild anything above.
COPY bundle /bundle
