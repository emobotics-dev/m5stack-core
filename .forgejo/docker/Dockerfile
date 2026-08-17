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
# Exposes the same /opt marker files altreg-ci / oxicharge-ci did (kept for
# backward-compat):
#   /opt/esp_libclang_path  /opt/esp_sysroot_path  /opt/esp_elf_bin_path
# AND — as of v3 (libclang path corrected in v4) — exports the full ESP
# cross-build environment (PATH,
# LIBCLANG_PATH, BINDGEN_EXTRA_CLANG_ARGS) so a consuming workflow needs no
# toolchain-plumbing step at all; see the ENV block at the end (#11).
# As of v5: also carries llvm-cov/llvm-profdata (version-matched to the `esp`
# toolchain's embedded LLVM, not rustup-installable for it) + cargo-llvm-cov,
# so a consuming workflow can run coverage without its own toolchain-plumbing
# step either — see the coverage-tooling RUN block near the end (#30). As of
# v6: the system-wide `llvm-cov`/`llvm-profdata` (unversioned, on PATH) also
# point at that same v21 copy instead of the libclang/bindgen `llvm` install's
# default LLVM 14 — a bare `llvm-cov --version` sanity check used to look like
# the fix hadn't landed. As of v7: the Xtensa toolchain is 1.97.0.0, the first
# esp release carrying the generic `Atomic<T>` that `mem::PsramSafe` needs; its
# embedded LLVM is still 21, so the coverage pin below is unaffected. As of v8:
# a nightly toolchain + miri and its prebuilt sysroot, for the host-crate leg.
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
        --toolchain-version 1.97.0.0 \
        --export-file /dev/null && \
    find "$RUSTUP_HOME/toolchains/esp" -maxdepth 8 \( -name "libclang.so" -o -name "libclang.so.*" \) \
        | head -1 | xargs -I{} dirname {} > /opt/esp_libclang_path && \
    find "$RUSTUP_HOME/toolchains/esp/xtensa-esp-elf" -maxdepth 8 -path "*/include/string.h" \
        -not -path "*/ssp/*" -not -path "*/sys/*" \
        | head -1 | xargs -I{} sh -c 'dirname $(dirname "{}")' > /opt/esp_sysroot_path && \
    find "$RUSTUP_HOME/toolchains/esp/xtensa-esp-elf" -maxdepth 4 -name "xtensa-esp32-elf-gcc" \
        | head -1 | xargs -I{} dirname {} > /opt/esp_elf_bin_path && \
    test -s /opt/esp_elf_bin_path || (echo "xtensa-esp32-elf-gcc not found under $RUSTUP_HOME/toolchains/esp" >&2 && exit 1) && \
    ELF_BIN="$(cat /opt/esp_elf_bin_path)" && \
    ln -sfn "$ELF_BIN" /opt/esp-bin && \
    ln -sfn "$(dirname "$(cat /opt/esp_libclang_path)")" /opt/esp-clang && \
    ln -sfn "$(realpath "$("$ELF_BIN/xtensa-esp32-elf-gcc" -print-sysroot)")" /opt/esp-sysroot && \
    test -e /opt/esp-sysroot/include/inttypes.h || \
        (echo "esp newlib sysroot at /opt/esp-sysroot lacks include/inttypes.h" >&2 && exit 1) && \
    ls /opt/esp-clang/lib/clang/*/include/stddef.h >/dev/null 2>&1 || \
        (echo "esp clang resource dir under /opt/esp-clang/lib lacks clang/*/include/stddef.h" >&2 && exit 1)

# Export the ESP cross-build environment so consuming CI jobs need ZERO toolchain
# plumbing (matches what the clco devcontainer wires into .bashrc). The stable
# /opt/esp-* symlinks above pin version-independent paths so these ENV values are
# static. Without this, every consumer re-derives them by hand and gets them
# subtly wrong two ways:
#   1. Sysroot: ESP clang's own inttypes.h does an `#include_next` that only
#      resolves inside the *doubled* xtensa-esp-elf newlib sysroot (`-print-sysroot`);
#      the single-level dir looks right but has no include/inttypes.h (see #11).
#      The explicit `-I…/include` makes bindgen resolve it regardless of search order.
#   2. libclang: LIBCLANG_PATH must point at `<clang-prefix>/lib` — clang derives
#      its resource dir (the builtin stddef.h/stdarg.h) as `dirname(LIBCLANG_PATH)`.
#      So the symlink is to the clang *prefix* (/opt/esp-clang) and the value is
#      /opt/esp-clang/lib; a symlink straight to the lib dir would make the prefix
#      resolve to /opt and clang would fail with `stddef.h file not found`.
ENV PATH="/opt/esp-bin:${PATH}" \
    LIBCLANG_PATH="/opt/esp-clang/lib" \
    BINDGEN_EXTRA_CLANG_ARGS="--sysroot=/opt/esp-sysroot -I/opt/esp-sysroot/include"

# Coverage tooling (#30): `cargo llvm-cov` needs llvm-cov/llvm-profdata, but
# `rustup component add llvm-tools-preview` cannot target the `esp` toolchain
# — it's an espup-installed custom toolchain (not one rustup distributes), so
# rustup has no component manifest for it, and Espressif's own toolchain
# releases (esp-rs/rust-build) ship no llvm-tools counterpart either. The
# coverage raw-profile format is tied to the LLVM version rustc embeds for
# codegen (`rustc +esp -vV` reports LLVM 21.1.3 as of this toolchain-version
# pin), so the tools must be version-matched, not just any llvm-cov found on
# the system. apt.llvm.org's llvm-21 packages track the same LLVM 21 major
# series — confirmed compatible with a real round trip (`rustc +esp -C
# instrument-coverage`, run, `llvm-profdata merge`, `llvm-cov report`), not
# just a version-number assumption. Installed to the esp toolchain's own
# sysroot bin dir — exactly where `llvm-tools-preview` would place them for a
# rustup-managed toolchain, and where `cargo llvm-cov` looks regardless of
# whether rustup thinks the component is present.
#
# Also repoints the system-wide `llvm-cov`/`llvm-profdata` (the earlier
# `apt-get install ... llvm` above ships these as plain symlinks to Debian's
# default LLVM 14, for libclang/bindgen's sake — nothing here needed the
# unversioned names before). Left alone, that's a landmine: `cargo llvm-cov`
# ignores it and correctly uses the sysroot copy above, but a bare `llvm-cov
# --version` sanity check finds LLVM 14 and looks like this fix didn't work
# (mistaken for real, cost real investigation time). Repointing both onto the
# v21 copy means there is exactly one answer in the image, not two.
RUN apt-get update -q && \
    apt-get install -y --no-install-recommends lsb-release software-properties-common gnupg wget && \
    curl -fsSL https://apt.llvm.org/llvm.sh | bash -s -- 21 && \
    apt-get install -y --no-install-recommends llvm-21 && \
    rm -rf /var/lib/apt/lists/* && \
    ESP_SYSROOT_BIN="$RUSTUP_HOME/toolchains/esp/lib/rustlib/x86_64-unknown-linux-gnu/bin" && \
    install -m 0755 /usr/lib/llvm-21/bin/llvm-cov "$ESP_SYSROOT_BIN/llvm-cov" && \
    install -m 0755 /usr/lib/llvm-21/bin/llvm-profdata "$ESP_SYSROOT_BIN/llvm-profdata" && \
    ln -sf /usr/lib/llvm-21/bin/llvm-cov /usr/bin/llvm-cov && \
    ln -sf /usr/lib/llvm-21/bin/llvm-profdata /usr/bin/llvm-profdata && \
    cargo install cargo-llvm-cov --locked

# Miri, for ci.yml's host-crate leg. A second toolchain, never used for firmware:
# miri ships only as a component of a rustup-distributed nightly, and the `esp`
# toolchain is espup-installed with no component manifest at all — the same wall
# the llvm-tools block above hit.
#
# The sysroot is prebuilt here rather than at run time. Miri interprets a
# MIR-only std it compiles itself, which costs ~2 min per run, and it caches
# under $HOME — which differs per runner user, so a run-time build would rarely
# hit. XDG_CACHE_HOME places it; MIRI_SYSROOT is how every later invocation
# finds it regardless of who is running.
RUN rustup toolchain install nightly --profile minimal \
        --component miri --component rust-src && \
    XDG_CACHE_HOME=/opt/miri-cache cargo +nightly miri setup && \
    chmod -R a+rX /opt/miri-cache

ENV MIRI_SYSROOT=/opt/miri-cache/miri

WORKDIR /workspace
