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
//!     // "" if you don't want a features tag in the mark.
//!     m5stack_core_build::emit_identity_env("csp");
//! }
//! ```
//!
//! `app_desc!()` (under `identity`) joins this with `CARGO_BIN_NAME` — set by
//! Cargo at the point `app_desc!()` itself expands, since a package can have
//! more than one `[[bin]]` and a `build.rs` (this crate) runs once per
//! *package*, not per binary, so it can never know which binary it's
//! describing. That's why the binary name isn't part of what this crate
//! emits: it genuinely can't be, correctly, from here.
//!
//! This crate performs **no length enforcement or truncation** — the full
//! mark (`CARGO_BIN_NAME` + this crate's output) is only known where
//! `app_desc!()` expands, so that's where the length is checked, as a real
//! compile error if it doesn't fit, not a silent cut here. Which part to
//! shorten in response (the `features` tag, or nothing this crate controls)
//! is entirely the caller's call.
//!
//! This crate only ever inspects the *calling* crate's own directory
//! (`CARGO_MANIFEST_DIR`, which is unambiguous for a build script — it is
//! always the directory of the crate whose `build.rs` is running). It never
//! touches `m5stack-core`'s own tree: content stays the consumer's, the BSP
//! owns only the mechanism that reads it back (see `m5stack_core::app_desc!`).

use std::path::Path;
use std::process::Command;

/// Sets `cargo:rustc-env=M5STACK_CORE_BUILD_MARK=<mark>` from `features` and
/// the calling crate's own git state: `<features>/<hash><dirty>`, or just
/// `<hash><dirty>` if `features` is `""` — an 8-hex-char abbreviated commit
/// hash, plus a trailing `*` if the working tree has uncommitted changes
/// (e.g. `csp/a1b2c3d4*`, or `a1b2c3d4*` with no features tag).
///
/// `features` is never inspected or validated — pass whatever short,
/// consumer-meaningful tag you want (or `""`); this crate has no way to know
/// which of your Cargo features are identity-relevant, so it doesn't guess.
///
/// Never fails the build: falls back to `"unknown"` for the commit if `git`
/// isn't on `PATH`, the crate isn't a git checkout (e.g. built from a source
/// tarball), or `git` errors for any other reason.
pub fn emit_identity_env(features: &str) {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is set by cargo for every build script");
    let commit = git_mark(Path::new(&dir)).unwrap_or_else(|| "unknown".to_string());
    let mark = join_mark(features, &commit);
    println!("cargo:rustc-env=M5STACK_CORE_BUILD_MARK={mark}");
    // Re-run only when the commit or working-tree state actually changes,
    // not on every build.
    println!("cargo:rerun-if-changed={dir}/.git/HEAD");
    println!("cargo:rerun-if-changed={dir}/.git/index");
}

fn join_mark(features: &str, commit: &str) -> String {
    if features.is_empty() { commit.to_string() } else { format!("{features}/{commit}") }
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

#[cfg(test)]
mod tests {
    use super::join_mark;

    #[test]
    fn no_features_is_just_the_commit() {
        assert_eq!(join_mark("", "a1b2c3d4*"), "a1b2c3d4*");
    }

    #[test]
    fn features_prefix_the_commit() {
        assert_eq!(join_mark("csp", "a1b2c3d4*"), "csp/a1b2c3d4*");
    }
}
