# SPDX-License-Identifier: BSD-3-Clause
# The m5stack-core CI image (registry:3000/emobotics/m5stack-core/m5stack-core-ci)
# — the reference ESP-BSP CI container: the dual-board (ESP32 Fire27 + ESP32-S3
# CoreS3) Xtensa Rust toolchain plus the LVGL/bindgen build deps this repo's
# examples need. Built + published by .forgejo/workflows/build-ci-image.yml;
# consumed by .forgejo/workflows/ci.yml.
#
# Adapted from emobotics/oxicharge's oxicharge-ci. What makes this the BSP
# reference image (and reusable by CoreS3-only consumers, which layer their own
# app runtime on top):
#   - espup installs BOTH esp32 and esp32s3 — m5stack-core is dual-board, so the
#     xtensa-esp32-elf-gcc linker (Fire27) must be present, not just the s3 one.
#     This is the exact gap that made oxicharge-ci (esp32s3-only) unusable here.
#   - cmake + libclang + pypng/lz4 for the `lvgl` example: oxivgl-sys runs bindgen
#     and LVGL's LVGLImage.py (imports the `png`/`lz4` modules) to convert a
#     bundled image asset.
#   - python3-dev + libffi-dev: generic native-extension build headers so a
#     consumer's `pip`/`poetry` can compile from-source Python packages (e.g.
#     cffi/cryptography) when no prebuilt wheel matches — same completeness class
#     as the pypng/lz4 tooling, not app-specific (see #5).
#   - no Josev/JRE/poetry runtime — that is oxicharge-app-specific; a consumer
#     needing it builds a thin layer FROM this image rather than bloating it here.
#
# Exposes the same /opt marker files altreg-ci / oxicharge-ci did, which
# ci.yml's "ESP libclang + sysroot + PATH" step reads:
#   /opt/esp_libclang_path  /opt/esp_sysroot_path  /opt/esp_elf_bin_path
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update -q && \
    apt-get install -y --no-install-recommends \
        git curl ca-certificates \
        build-essential pkg-config cmake \
        libclang-dev llvm \
        python3 python3-venv python3-pip python3-dev libffi-dev && \
    pip3 install --break-system-packages --no-cache-dir pypng lz4 && \
    rm -rf /var/lib/apt/lists/*

# Node 22 — bookworm's apt only carries 18.x, below actions/checkout@v4's node20+
# floor. The rust/esp32 runner labels' default images carry no Node, so it has to
# be baked in rather than hand-rolling a git clone in every workflow.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
    apt-get install -y --no-install-recommends nodejs && \
    rm -rf /var/lib/apt/lists/*

# Rust + espup under /opt so the toolchain is shared regardless of which user the
# runner starts the job container as (matches altreg-ci / oxicharge-ci layout;
# RUSTUP_HOME/CARGO_HOME are also set in the consuming workflow's env).
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# Stable builds espup; espup installs the `esp` channel + rust-src + the Xtensa
# GCC linkers for BOTH boards (xtensa-esp32-elf-gcc for Fire27,
# xtensa-esp32s3-elf-gcc for CoreS3 — .cargo/config.toml names both). Then record
# the libclang dir, newlib sysroot and gcc bin dir into the /opt marker files the
# CI "ESP libclang + sysroot + PATH" step reads (espup exposes no fixed path).
# `find ... -name xtensa-esp32-elf-gcc` for the bin marker fails the build if the
# esp32 (Fire27) linker is somehow absent — the very gap this image exists to close.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --no-modify-path --default-toolchain stable && \
    cargo install espup && \
    espup install --targets esp32,esp32s3 \
        --toolchain-version 1.95.0.0 \
        --export-file /dev/null && \
    find "$RUSTUP_HOME/toolchains/esp" -maxdepth 8 \( -name "libclang.so" -o -name "libclang.so.*" \) \
        | head -1 | xargs -I{} dirname {} > /opt/esp_libclang_path && \
    find "$RUSTUP_HOME/toolchains/esp/xtensa-esp-elf" -maxdepth 8 -path "*/include/string.h" \
        -not -path "*/ssp/*" -not -path "*/sys/*" \
        | head -1 | xargs -I{} sh -c 'dirname $(dirname "{}")' > /opt/esp_sysroot_path && \
    find "$RUSTUP_HOME/toolchains/esp/xtensa-esp-elf" -maxdepth 4 -name "xtensa-esp32-elf-gcc" \
        | head -1 | xargs -I{} dirname {} > /opt/esp_elf_bin_path && \
    test -s /opt/esp_elf_bin_path || (echo "xtensa-esp32-elf-gcc not found under $RUSTUP_HOME/toolchains/esp" >&2 && exit 1)

WORKDIR /workspace
