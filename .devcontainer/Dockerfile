# syntax=docker/dockerfile:1

# Stage 1: pull the prebuilt ESP Rust toolchain out of the CI image (same image
# m5stack-core CI runs in), so we don't spend 10-30 min on `espup install`.
# Keep the `:v2` tag in lockstep with .forgejo/workflows/{ci,build-ci-image}.yml.
FROM registry:3000/emobotics/m5stack-core/m5stack-core-ci:v2 AS esp-toolchain

# Stage 2: the clco base container + flashing tools.
# Despite the image name, this is NOT a Claude Code redistribution — it's the clco
# dev environment (source: emobotics/claude-container): fj/fj-ex, RTK, gh and the
# clco conventions, with Claude Code installed via the official installer
# (claude.ai/install.sh). Built + published to registry:3000, rebuilt weekly;
# org-internal tooling with no upstream image, so it lives here like the other
# clco/CI images. Same base every clco repo's devcontainer uses.
FROM registry:3000/emobotics/claude-code:latest

USER root

# ── System packages (only what's NOT in base image) ──────────────
# libudev-dev: required by espflash / probe-rs for serial + USB enumeration.
# python3 + pypng + lz4: the `lvgl` example's oxivgl-sys/oxivgl build scripts
#   run LVGL's `LVGLImage.py` (converts a bundled image asset), which imports
#   the `png` (pypng) and `lz4` modules.
RUN apt-get update && apt-get install -y --no-install-recommends \
        libudev-dev \
        python3 python3-pip \
    && pip3 install --no-cache-dir --break-system-packages pypng lz4 \
    && rm -rf /var/lib/apt/lists/*

# ── ESP Rust toolchain (copied from CI image) ────────────────────
# The Xtensa rustc (channel `esp`, pinned in the CI base image) plus the
# xtensa-esp-elf GCC. `build-std` compiles core/alloc from source, so no
# prebuilt target std is needed — only the toolchain and the linkers.
# The `lvgl` example uses bindgen (via oxivgl-sys). oxivgl-sys auto-detects the
# ESP clang under $RUSTUP_HOME, but bindgen still needs the newlib sysroot so
# clang's `#include_next <inttypes.h>` resolves — wired into .clco-env below.
COPY --from=esp-toolchain /opt/rustup           /opt/rustup
COPY --from=esp-toolchain /opt/cargo            /opt/cargo
COPY --from=esp-toolchain /opt/esp_elf_bin_path /opt/esp_elf_bin_path

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH="/opt/cargo/bin:${PATH}"

# Let claude write cargo caches at runtime; keep rustup world-readable.
RUN chown -R claude:claude /opt/cargo \
    && chmod -R a+rX /opt/rustup

# ── espflash + probe-rs (rebuild against the base image's glibc) ──
# Binaries copied from the Ubuntu-24.04 CI image are glibc-incompatible with
# this Debian base, so reinstall the host flashing tools. espflash 4.3.0 has a
# known nix-dependency build bug → install from git.
USER claude
RUN rm -f "$CARGO_HOME/bin/espflash" "$CARGO_HOME/bin/probe-rs" \
       "$CARGO_HOME/bin/cargo-flash" "$CARGO_HOME/bin/cargo-embed" \
       "$CARGO_HOME/.crates.toml" "$CARGO_HOME/.crates2.json" \
    && cargo install --git https://github.com/esp-rs/espflash espflash \
    && cargo install probe-rs-tools \
    && rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git" "$CARGO_HOME/.package-cache"

# ── Wire the linker dir onto PATH in .clco-env AND .bashrc ────────
# /opt/esp_elf_bin_path holds xtensa-esp32-elf-gcc / xtensa-esp32s3-elf-gcc,
# referenced as linkers by .cargo/config.toml.
RUN bash -c '\
    ELF_BIN=$(cat /opt/esp_elf_bin_path); \
    LINE="export PATH=\"$ELF_BIN:\$PATH\""; \
    echo "$LINE" >> /home/claude/.clco-env; \
    echo "$LINE" >> /home/claude/.bashrc'

# ── bindgen sysroot for the `lvgl` example (oxivgl-sys) ───────────
# Derive the newlib sysroot from the GCC itself (version-independent) and
# expose it to bindgen via BINDGEN_EXTRA_CLANG_ARGS. Without it, the ESP clang's
# bundled inttypes.h `#include_next`s a libc header it can't find.
RUN bash -c '\
    ELF_BIN=$(cat /opt/esp_elf_bin_path); \
    SYSROOT=$("$ELF_BIN/xtensa-esp32-elf-gcc" -print-sysroot); \
    LINE="export BINDGEN_EXTRA_CLANG_ARGS=\"--sysroot=$SYSROOT -I$SYSROOT/include\""; \
    echo "$LINE" >> /home/claude/.clco-env; \
    echo "$LINE" >> /home/claude/.bashrc'

USER claude
WORKDIR /workspace
