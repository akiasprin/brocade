# The release ABI follows .github/workflows/ci.yml, not the moving Cube development image.
ARG BASE_IMAGE=ubuntu:22.04
FROM ${BASE_IMAGE} AS base

ARG DEBIAN_FRONTEND=noninteractive
RUN set -eux; \
    apt-get update -o Acquire::Retries=3; \
    apt-get install -y --no-install-recommends \
      binutils \
      build-essential \
      ca-certificates \
      curl \
      file \
      gcc-aarch64-linux-gnu \
      git \
      libc6-dev-arm64-cross \
      pkg-config \
      python3 \
      python3-pip \
      python3-venv \
      xz-utils; \
    rm -rf /var/lib/apt/lists/*; \
    test "$(uname -m)" = x86_64; \
    test "$(. /etc/os-release; printf '%s' "$VERSION_ID")" = 22.04; \
    ldd --version; \
    aarch64-linux-gnu-gcc --version

FROM base AS release
RUN git --version && python3 --version && readelf --version

FROM base AS cube
COPY components/cube/docker/llvm-snapshot.gpg.key /etc/apt/trusted.gpg.d/apt.llvm.org.asc
RUN set -eux; \
    echo 'deb https://apt.llvm.org/jammy/ llvm-toolchain-jammy-21 main' \
      > /etc/apt/sources.list.d/llvm-21.list; \
    apt-get update -o Acquire::Retries=3; \
    apt-get install -y --no-install-recommends \
      clang-21 \
      cmake \
      e2fsprogs \
      libbpf-dev \
      libelf-dev \
      llvm-21 \
      zlib1g-dev; \
    rm -rf /var/lib/apt/lists/*; \
    clang-21 --version; \
    llvm-objcopy-21 --version; \
    debugfs -V; \
    test -f /usr/include/bpf/bpf_helpers.h

FROM cube AS packager
RUN python3 --version && debugfs -V && readelf --version
