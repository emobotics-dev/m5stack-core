// SPDX-License-Identifier: MIT OR Apache-2.0
//! Build-time helper for `m5stack-core`'s `identity` feature.
//!
//! Call [`emit_identity_env`] once from a consumer's own `build.rs` to set
//! `M5STACK_CORE_BUILD_MARK` — the env var `m5stack_core::app_desc!()`
//! requires when the BSP's `identity` feature is enabled. Host-only and `std`:
//! this is *build*-time infrastructure, and nothing it depends on is ever
//! linked into firmware — its one dependency (`vergen-gitcl`, for reading git
//! state) is compiled for the build machine and discarded.
//!
//! ```no_run
//! fn main() {
//!     // "" for no features tag; 12 matches this crate's usual abbreviation —
//!     // shorten it if your package/binary names leave no room to spare.
//!     m5stack_core_build::emit_identity_env("csp", 12);
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
//! The git state read is that of the repository containing the **calling**
//! crate — `git` is run from the build script's own directory and resolves
//! upwards from there. It never touches `m5stack-core`'s tree: content stays
//! the consumer's, and the BSP owns only the mechanism that reads it back (see
//! `m5stack_core::app_desc!`).

// The doc example above keeps its `fn main()` deliberately: what it documents
// is a `build.rs`, and a build script IS its `main`. Stripping it to satisfy
// the lint would show a reader a snippet that does not go anywhere real.
#![allow(clippy::needless_doctest_main, reason = "the example is a build script; its main is the subject")]

use vergen_gitcl::{Emitter, GitclBuilder};

/// The narrowest abbreviation this will produce.
///
/// Four, because that is what `git rev-parse --short` enforced before this
/// crate stopped shelling out, and silently changing what a caller's existing
/// `hash_len` means is worse than keeping a floor nobody asked about.
const MIN_HASH_LEN: usize = 4;

/// A full `sha1`, for bounding a request that asks for more than exists.
const SHA_LEN: usize = 40;

/// Sets `cargo:rustc-env=M5STACK_CORE_BUILD_MARK=<mark>` from `features` and
/// the calling crate's own git state: `<features>/<hash><dirty>`, or just
/// `<hash><dirty>` if `features` is `""` — an abbreviated commit hash
/// **exactly** `hash_len` hex characters wide (clamped to a floor of 4, which
/// is what `git rev-parse --short` enforced when this crate still shelled out
/// to it), plus a trailing `+` if the working tree has uncommitted or untracked
/// changes (e.g. `crypto-opt/0f63a4926303+` at `hash_len: 12`, or `0f63a4+` at
/// `hash_len: 6` with no features tag).
///
/// **Exactly**, and that is a deliberate change from `--short`, which treats
/// its argument as a minimum and lengthens the prefix when it would be
/// ambiguous. A fixed width is what the 31-byte `EspAppDesc::version` budget
/// needs — see [`abbreviate`] for the argument, and for what is given up.
///
/// The calling build script re-runs only when git state actually changes:
/// `vergen` emits `rerun-if-changed` for the **repository's** real `.git/HEAD`
/// and current branch ref, which a path built from `CARGO_MANIFEST_DIR` gets
/// wrong for any crate that is not the repository root.
///
/// `features` is never inspected or validated — pass whatever short,
/// consumer-meaningful tag you want (or `""`); this crate has no way to know
/// which of your Cargo features are identity-relevant, so it doesn't guess.
/// `hash_len` is yours to shorten too, for the same reason: if your
/// package/binary names (see `m5stack_core::app_desc!`) leave little room in
/// the 31-byte field, that's the lever, not a silent cut here.
///
/// Never fails the build: falls back to `"unknown"` for the commit if `git`
/// isn't on `PATH`, the crate isn't a git checkout (e.g. built from a source
/// tarball), or `git` errors for any other reason.
pub fn emit_identity_env(features: &str, hash_len: usize) {
    let commit = git_mark(hash_len).unwrap_or_else(|| "unknown".to_string());
    let mark = join_mark(features, &commit);
    println!("cargo:rustc-env=M5STACK_CORE_BUILD_MARK={mark}");
}

fn join_mark(features: &str, commit: &str) -> String {
    if features.is_empty() { commit.to_string() } else { format!("{features}/{commit}") }
}

/// Ask `vergen` for this build's git state, and abbreviate it.
///
/// `emit_and_set` does two things at once, and both are wanted: it prints the
/// `cargo:` instructions — including the `rerun-if-changed` paths that are the
/// reason this crate no longer drives `git` itself — and it sets the values in
/// this process so the mark can be composed from them.
///
/// It also leaves `VERGEN_GIT_SHA` and `VERGEN_GIT_DIRTY` set for the calling
/// crate. That is a side effect rather than a promise: build against
/// `M5STACK_CORE_BUILD_MARK`, which is the contract `app_desc!()` reads.
///
/// `None` — and therefore a mark of `unknown` — whenever the state cannot be
/// established: no `git` on `PATH`, not a checkout (a source tarball), or
/// `vergen` reporting its idempotent placeholder rather than a hash. Never a
/// failed build; that was true before and stays true.
fn git_mark(hash_len: usize) -> Option<String> {
    let gitcl = GitclBuilder::default()
        // false = the FULL sha. The abbreviation is done here, deliberately —
        // see `abbreviate`.
        .sha(false)
        // `true` = count untracked files as dirty, matching what the previous
        // `git status --porcelain` did. Changing it would silently redefine
        // what a `+` on the end of a mark means.
        .dirty(true)
        .build()
        .ok()?;
    Emitter::default().add_instructions(&gitcl).ok()?.emit_and_set().ok()?;

    let sha = std::env::var("VERGEN_GIT_SHA").ok()?;
    let dirty = std::env::var("VERGEN_GIT_DIRTY").is_ok_and(|d| d == "true");
    abbreviate(&sha, hash_len, dirty)
}

/// `<hash><dirty>` — the commit half of a mark.
///
/// Returns `None` for anything that is not a hash, which is how `vergen`'s
/// `VERGEN_IDEMPOTENT_OUTPUT` placeholder (emitted when it cannot determine the
/// value) is kept out of a firmware's identity instead of being baked in as if
/// it meant something.
///
/// ## Exactly `hash_len`, which `git rev-parse --short` did not guarantee
///
/// `--short=<n>` treats `n` as a **minimum**: git lengthens the prefix when `n`
/// characters would be ambiguous in that repository. Truncating here gives a
/// width that is fixed, and that is the property this particular field needs —
/// the mark goes into `EspAppDesc::version`, whose 31-byte ceiling
/// `app_desc!()` enforces with a `const` assertion. A hash that quietly grew as
/// a repository aged would turn into a compile error in somebody else's crate,
/// at a width they had already tested.
///
/// The cost is that a prefix is no longer guaranteed unique. At the documented
/// 12 characters that is not a practical concern, and a mark is provenance for
/// a human rather than a lookup key — the harness compares the ELF's full
/// `sha256`, not this.
fn abbreviate(sha: &str, hash_len: usize, dirty: bool) -> Option<String> {
    if sha.is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let width = hash_len.clamp(MIN_HASH_LEN, SHA_LEN);
    let short: String = sha.chars().take(width).collect();
    Some(if dirty { format!("{short}+") } else { short })
}

/// The mark is composed from two pure functions so it can be tested without a
/// git checkout and, more importantly, **without calling `vergen`**.
///
/// `emit_and_set` sets process environment variables. `std::env::set_var` is
/// unsound while other threads run, and cargo's test harness is multi-threaded
/// by default — so a test that exercised the `vergen` call directly would be
/// introducing a data race to check a wiring detail. The wiring is verified
/// where it actually runs instead: by building a consumer and reading back both
/// the mark it produced and whether a second, untouched build re-ran the script.
#[cfg(test)]
mod tests {
    use super::{MIN_HASH_LEN, SHA_LEN, abbreviate, join_mark};

    /// A real commit hash, so the width arithmetic is exercised against the
    /// length these actually are.
    const SHA: &str = "0f63a49263039a4b1c2d3e4f5a6b7c8d9e0f1a2b";

    #[test]
    fn no_features_is_just_the_commit() {
        assert_eq!(join_mark("", "0f63a4926303+"), "0f63a4926303+");
    }

    #[test]
    fn features_prefix_the_commit() {
        assert_eq!(join_mark("crypto-opt", "0f63a4926303+"), "crypto-opt/0f63a4926303+");
    }

    /// The property `--short` did NOT give: the width asked for is the width
    /// produced, so a caller's 31-byte budget cannot be spent by a repository
    /// growing an ambiguous prefix.
    #[test]
    fn the_width_asked_for_is_the_width_produced() {
        for hash_len in [4, 6, 8, 12, 16, 40] {
            let mark = abbreviate(SHA, hash_len, false).expect("a hex sha abbreviates");
            assert_eq!(mark.len(), hash_len, "hash_len={hash_len} produced {mark:?}");
            assert!(SHA.starts_with(&mark), "must be a PREFIX of the real hash: {mark:?}");
        }
    }

    /// Below git's old floor the result is clamped rather than silently
    /// honoured — a 1-character "hash" identifies nothing, and this is the
    /// width callers have been getting all along.
    #[test]
    fn an_absurdly_short_request_is_clamped_to_the_old_git_floor() {
        for hash_len in [0, 1, 3] {
            assert_eq!(abbreviate(SHA, hash_len, false).expect("abbreviates").len(), MIN_HASH_LEN);
        }
    }

    /// Asking for more than a sha1 has yields the whole thing, not padding.
    #[test]
    fn a_request_longer_than_the_hash_is_bounded_by_it() {
        assert_eq!(abbreviate(SHA, 1000, false).expect("abbreviates"), SHA);
        assert_eq!(SHA.len(), SHA_LEN, "the fixture must be a real sha1 width");
    }

    #[test]
    fn a_dirty_tree_is_marked_and_does_not_eat_a_hash_character() {
        let mark = abbreviate(SHA, 12, true).expect("abbreviates");
        assert_eq!(mark, "0f63a4926303+");
        assert_eq!(mark.trim_end_matches('+').len(), 12, "the + is extra, not part of the width");
    }

    /// THE guard that keeps a non-answer out of a firmware's identity. vergen
    /// emits `VERGEN_IDEMPOTENT_OUTPUT` when it cannot determine a value; baking
    /// that in would produce a build that confidently names itself something
    /// meaningless, which is worse than admitting `unknown`.
    #[test]
    fn a_placeholder_or_junk_is_refused_rather_than_abbreviated() {
        assert_eq!(abbreviate("VERGEN_IDEMPOTENT_OUTPUT", 12, false), None);
        assert_eq!(abbreviate("", 12, false), None);
        assert_eq!(abbreviate("not-a-hash", 12, false), None);
    }
}
