// SPDX-License-Identifier: MIT OR Apache-2.0
//! Build-time helper for `m5stack-core`'s `identity` feature.
//!
//! Call [`emit_identity_env`] once from a consumer's own `build.rs` to set
//! `M5STACK_CORE_BUILD_MARK` — the env var `m5stack_core::app_desc!()`
//! requires when the BSP's `identity` feature is enabled. Host-only, `std`,
//! zero embedded dependencies: this is *build*-time infrastructure, never
//! linked into firmware.
//!
//! ```no_run
//! fn main() {
//!     m5stack_core_build::emit_identity_env();
//! }
//! ```
//!
//! This crate only ever inspects the *calling* crate's own directory
//! (`CARGO_MANIFEST_DIR`, which is unambiguous for a build script — it is
//! always the directory of the crate whose `build.rs` is running). It never
//! touches `m5stack-core`'s own tree: content stays the consumer's, the BSP
//! owns only the mechanism that reads it back (see `m5stack_core::app_desc!`).

use std::path::Path;
use std::process::Command;

/// Bytes budget for the emitted mark. `EspAppDesc::version` (the field
/// `app_desc!()` writes this into under `identity`) is a fixed 32-byte C
/// string that silently truncates past that, and a consumer may want to
/// prefix the mark with their own `CARGO_PKG_VERSION` — keep this short.
const MAX_MARK_LEN: usize = 16;

/// Sets `cargo:rustc-env=M5STACK_CORE_BUILD_MARK=<mark>` from the calling
/// crate's own git state: an 8-hex-char abbreviated commit hash, plus a
/// trailing `*` if the working tree has uncommitted changes (e.g. `a1b2c3d4*`).
///
/// Never fails the build: falls back to `"unknown"` if `git` isn't on `PATH`,
/// the crate isn't a git checkout (e.g. built from a source tarball), or `git`
/// errors for any other reason.
pub fn emit_identity_env() {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is set by cargo for every build script");
    let mark = git_mark(Path::new(&dir)).unwrap_or_else(|| "unknown".to_string());
    let mark = truncate(&mark, MAX_MARK_LEN);
    println!("cargo:rustc-env=M5STACK_CORE_BUILD_MARK={mark}");
    // Re-run only when the commit or working-tree state actually changes,
    // not on every build.
    println!("cargo:rerun-if-changed={dir}/.git/HEAD");
    println!("cargo:rerun-if-changed={dir}/.git/index");
}

fn git_mark(dir: &Path) -> Option<String> {
    let hash = run_git(dir, &["rev-parse", "--short=8", "HEAD"])?;
    let dirty = !run_git(dir, &["status", "--porcelain"])?.is_empty();
    Some(if dirty { format!("{hash}*") } else { hash })
}

fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
}

fn truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("a1b2c3d4", 16), "a1b2c3d4");
    }

    #[test]
    fn truncate_long_string_cuts_at_budget() {
        assert_eq!(truncate("a1b2c3d4*-N-g0123456789", 16), "a1b2c3d4*-N-g012");
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        let s = "0123456789abcdef";
        assert_eq!(truncate(s, 16), s);
    }
}
