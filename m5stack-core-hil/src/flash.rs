// SPDX-License-Identifier: MIT OR Apache-2.0
//! Writing an image only when the board is not already running it.
//!
//! Boundary: **deciding whether this board needs this image, acting on that,
//! and proving the action took.** Reading the identity fails that sentence
//! ([`crate::board`]).
//!
//! [`ensure_image`] resets, reads what the board is running, writes only if it
//! differs, and **re-verifies afterwards**. A match costs ~6 s against ~15 s
//! for a write, so asking is worth it. It replaces a stamp file, which answers
//! *what do we believe we flashed?* and is wrong whenever a flash went around
//! the guard — [`crate::identity`] argues that at length.
//!
//! The hash is computed here rather than read from the ELF, because the
//! descriptor's `app_elf_sha256` is all-zero in a linked ELF. The premise that
//! `espflash` then computes it over the file and patches it in at flash time
//! is **false for espflash 4.x**: it patches nothing, so the field is still
//! thirty-two zero bytes in the image that reaches the board. Verified by
//! hexdump of `espflash save-image` output — descriptor magic `0xABCD5432` at
//! `0x20`, hash field at `0xb0` all zero.
//!
//! [`ExpectedImage::matched_by`] therefore compares the hash only when the
//! board reports a nonzero one, and says what that costs. Do not restore the
//! unconditional comparison without first checking that a flashed image
//! actually carries a hash.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use crate::{
    board::{self, Board, Capture},
    identity::{self, Identity},
    listen::{Listener, Source},
    serial::{self, DrainedSource, SerialSource},
    wait,
};

/// The identity an image file *should* produce once it is on a board.
///
/// Carries **both** descriptor name fields, because what the board prints as
/// its "mark" depends on how the firmware was built, and an ELF cannot tell
/// which from one field alone:
///
/// - with `m5stack-core`'s `identity` feature, the mark is the descriptor's
///   `version` (the git build mark) and `version=` carries the crate version;
/// - without it, the mark is the descriptor's `project_name` (the binary name)
///   and `version=` carries the descriptor's `version`.
///
/// Both shapes are checked explicitly rather than guessed at — see
/// [`ExpectedImage::matched_by`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedImage {
    /// The descriptor's `version` field.
    pub version: String,
    /// The descriptor's `project_name` field.
    pub project_name: String,
    /// The leading hex of `sha256` over the ELF file, truncated to the width
    /// the board prints.
    pub sha256_prefix: String,
}

impl ExpectedImage {
    /// Does `have` describe this image?
    ///
    /// The name half is accepted in **either** of the two shapes above, and in
    /// the plain shape the reported `version=` is checked too, so a match pins
    /// both descriptor fields rather than one.
    ///
    /// # The hash is compared only when the board reports one
    ///
    /// `espflash` 4.x does **not** populate `app_elf_sha256`: in the image it
    /// writes, the descriptor magic is correct at `0x20` and the hash field at
    /// `0xb0` is thirty-two zero bytes. A board therefore reports
    /// `app_elf_sha256=000000000000` truthfully, while [`identity_of_elf`]
    /// computes `sha256` over the ELF *file* — two different things that can
    /// never be equal. Comparing them unconditionally failed every
    /// verification with "flash did not take" on an image that had just been
    /// written correctly (`m5stack-core#68`; hit independently on ESP32 and
    /// ESP32-S3).
    ///
    /// So an all-zero reported hash means *the board has no content hash to
    /// offer*, and the comparison rests on the mark. A **nonzero** hash is
    /// still compared, so this re-tightens by itself if `espflash` starts
    /// patching the field again — no code change needed, and no version sniff.
    ///
    /// State the cost plainly, and **for both shapes `matched_by` accepts, not
    /// just one**: while the field stays zero, the gate cannot see two
    /// different builds sharing the same mark.
    ///
    /// For an `identity` build that gap is narrow — the mark carries
    /// pkg/bin/feature-arm/commit and a `+` for an uncommitted tree, so only
    /// two builds of the *same commit with the same dirty state* are
    /// indistinguishable. For a **plain** build the mark is just the binary
    /// name and `version=` is the crate's own `Cargo.toml` version, neither of
    /// which moves with a source edit that leaves the version string
    /// unbumped — the ordinary case in development. So a plain build's gap is
    /// wide: any two builds sharing a binary name and version string match,
    /// with nothing left to tell them apart. Reason enough that it is not
    /// nothing, and it is why this is a bug against `espflash` rather than a
    /// design choice here.
    #[must_use]
    pub fn matched_by(&self, have: &Identity) -> bool {
        if !have.hash_is_unpatched() && have.sha256_prefix != self.sha256_prefix {
            return false;
        }
        // `identity` build: the mark IS the descriptor's version field.
        let as_identity = have.mark == self.version;
        // Plain build: the mark is the binary name, and `version=` is the
        // descriptor's version.
        let as_plain = have.mark == self.project_name && have.version == self.version;
        as_identity || as_plain
    }
}

impl core::fmt::Display for ExpectedImage {
    /// **Labelled, deliberately not mirroring** what a board prints.
    ///
    /// Which shape a board uses depends on whether its firmware carries the
    /// `identity` feature, and `fmt` has no [`Identity`] to branch on — only
    /// [`ExpectedImage::matched_by`] does. Rendering one shape unconditionally
    /// therefore mislabels the other: an `identity` build came out as `secc
    /// version=oxichg/secc/c/82962280`, i.e. the binary name where the mark
    /// goes and the mark after `version=`, which reads as a corrupted value in
    /// the one line a person uses to judge whether a write was warranted.
    ///
    /// Labels make the two lines honestly different shapes rather than the same
    /// shape with the values shuffled.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "project_name={} version={} app_elf_sha256={}",
            self.project_name, self.version, self.sha256_prefix
        )
    }
}

/// How to write an image to this board.
///
/// Everything here is a per-project fact rather than a property of the harness,
/// so it is stated by the caller rather than guessed.
#[derive(Debug, Clone)]
pub struct FlashConfig {
    /// `espflash`'s `--chip`, e.g. `esp32s3`.
    pub chip: String,
    /// `--flash-size`, e.g. `16mb`. `None` leaves espflash's default.
    pub flash_size: Option<String>,
    /// `--flash-freq`.
    ///
    /// Defaults to `80mhz` in [`FlashConfig::cores3`] deliberately: espflash's
    /// own default is 40 MHz, code runs from flash by XIP, and a measurement
    /// must not silently depend on which tool wrote the image.
    pub flash_freq: Option<String>,
    /// `--partition-table`. `None` uses espflash's built-in layout.
    pub partition_table: Option<PathBuf>,
}

impl FlashConfig {
    /// The CoreS3 defaults this repo builds with.
    #[must_use]
    pub fn cores3() -> Self {
        Self { chip: "esp32s3".into(), flash_size: None, flash_freq: Some("80mhz".into()), partition_table: None }
    }

    /// The Fire27 defaults: a plain ESP32, same 80 MHz reasoning as above.
    ///
    /// If a freshly written Fire27 does not boot, `flash_freq` is the first
    /// thing to try at `40mhz` — espflash's own default, and the safe value for
    /// a flash part that cannot run at 80. That is a one-line `hil.toml` edit
    /// rather than a code change, and the failure is loud: `--ensure-image`
    /// verifies the write took by reading the board back, so a board that does
    /// not come up is reported and not measured.
    #[must_use]
    pub fn fire27() -> Self {
        Self { chip: "esp32".into(), flash_size: None, flash_freq: Some("80mhz".into()), partition_table: None }
    }

    /// Defaults for a named `espflash` chip, falling back to the CoreS3's for
    /// anything this crate has no opinion about — with `chip` set as asked, so
    /// an unknown target is still addressable rather than silently rewritten.
    #[must_use]
    pub fn for_chip(chip: &str) -> Self {
        match chip {
            "esp32" => Self::fire27(),
            "esp32s3" => Self::cores3(),
            other => Self { chip: other.to_string(), ..Self::cores3() },
        }
    }
}

/// What [`ensure_image`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ensured {
    /// The board already ran the image; nothing was written.
    AlreadyCurrent(Identity),
    /// The image was written and the board then confirmed it, with the reason
    /// the write was spent.
    Flashed { why: String, now: Identity },
}

/// The identity an ELF should produce: its descriptor mark, plus `sha256` of
/// the file truncated to `hash_prefix_len` hex characters.
///
/// `sha256sum` is shelled out to for the same reason `stty` and `espflash` are:
/// the run logic takes no dependencies, and coreutils is already a hard
/// requirement of anything driving a board. (The manifest's three are for
/// parsing a command line and a config file — commodities with obvious crates;
/// a hash is not one of them here, because the tool is already present.)
///
/// # Errors
/// If the file cannot be read, carries no application descriptor, or
/// `sha256sum` cannot be run.
pub fn identity_of_elf(elf: &Path, hash_prefix_len: usize) -> Result<ExpectedImage, String> {
    let bytes = std::fs::read(elf).map_err(|e| format!("cannot read {}: {e}", elf.display()))?;
    let desc = identity::from_elf(&bytes).ok_or_else(|| {
        format!(
            "no esp-idf application descriptor in {} — was it built with `m5stack_core::app_desc!()`?",
            elf.display()
        )
    })?;

    let out = Command::new("sha256sum").arg(elf).output().map_err(|e| format!("cannot run sha256sum: {e}"))?;
    if !out.status.success() {
        return Err(format!("sha256sum failed for {}", elf.display()));
    }
    let full = String::from_utf8_lossy(&out.stdout);
    let hex = full.split_whitespace().next().unwrap_or_default();
    let prefix = hex
        .get(..hash_prefix_len)
        .ok_or_else(|| format!("sha256sum gave only {} hex chars for {}", hex.len(), elf.display()))?;

    Ok(ExpectedImage {
        version: desc.version,
        project_name: desc.project_name,
        sha256_prefix: prefix.to_ascii_lowercase(),
    })
}

/// Write `elf` to the board.
///
/// The caller must not hold the port — espflash needs it exclusively.
/// [`Listener::across_reset`] is what guarantees that.
///
/// # Errors
/// If espflash cannot be run, or reports failure.
pub fn flash_image(cfg: &FlashConfig, port: &str, elf: &Path) -> Result<(), String> {
    let mut cmd = Command::new("espflash");
    cmd.args(["flash", "--chip", &cfg.chip, "--port", port, "--non-interactive"]);
    if let Some(t) = &cfg.partition_table {
        cmd.arg("--partition-table").arg(t);
    }
    if let Some(s) = &cfg.flash_size {
        cmd.args(["--flash-size", s]);
    }
    if let Some(f) = &cfg.flash_freq {
        cmd.args(["--flash-freq", f]);
    }
    let out = cmd.arg(elf).output().map_err(|e| format!("cannot run espflash: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "espflash could not write {} to {port}: {}",
        elf.display(),
        err.lines().rev().take(5).collect::<Vec<_>>().join(" | ")
    ))
}

/// Make sure `board` is running `elf`, writing it only if it is not.
///
/// `l` must already have been reset by the caller and have had its identity
/// read — this consumes that capture rather than causing a second reset.
///
/// # Errors
/// If the port dies, the write fails, or the board does not report the expected
/// image afterwards. A capture that cannot be trusted is **not** an error here:
/// it is a reason to write the image.
pub fn ensure_image(
    l: &mut Listener<DrainedSource>,
    board: &Board,
    cfg: &FlashConfig,
    elf: &Path,
    capture: &Capture,
    banner: Option<&str>,
    hash_prefix_len: usize,
) -> Result<Ensured, String> {
    let want = identity_of_elf(elf, hash_prefix_len)?;
    let hole = board::console_hole(l);

    // Every reason to flash, decided before touching anything, so the log says
    // WHY a 40 s write is being spent. Ordered so it says the most useful
    // thing: a holed capture and a board that never booted are both "nothing is
    // proved", and the answer to that is to write the image, not to give up.
    let why = match (capture, hole.as_deref()) {
        (_, Some(marker)) => Some(format!("the capture has a hole ({marker}), so nothing is proved")),
        (Capture::NoApplication, _) => {
            Some("the board never reached the application — a bad or erased image".to_string())
        }
        (Capture::NoIdentity, _) => Some("the board booted but printed no identity".to_string()),
        (Capture::Identified(have), _) if !want.matched_by(have) => Some(format!("board has {have}, image is {want}")),
        (Capture::Identified(have), _) => return Ok(Ensured::AlreadyCurrent(have.clone())),
    };
    let why = why.unwrap_or_else(|| "unknown".to_string());

    // The port must be released for espflash, which opens it exclusively.
    let (b, e) = (board.clone(), elf.to_path_buf());
    let cfg = cfg.clone();
    l.across_reset(|| {
        // A board carrying a bad image re-enumerates repeatedly, so espflash
        // must not be pointed at it mid-cycle. The path is stable, so the only
        // question is whether the device is present right now.
        //
        // `openable` is the RIGHT probe here, unlike below: what follows is
        // `espflash`, which wants the port to itself and does not care what the
        // board said beforehand. Discarding those bytes costs nothing. The
        // distinction is the whole rule — a throwaway open is honest in front
        // of a *tool*, and destructive in front of a *reader*.
        wait::until(
            &format!("board {} to be addressable for flashing", b.id),
            board::RETURN_BUDGET,
            board::RETURN_GAP,
            || SerialSource::openable(&b.port),
        )?;
        flash_image(&cfg, &b.port, &e)?;
        // Re-attach WITHOUT resetting. espflash resets the board itself after a
        // write, but that reset happens while nobody holds the port — and the
        // USB-Serial-JTAG discards output with no host attached, so the boot
        // this produces is unobservable. Take the port back first, then reset
        // deliberately below with the listener attached.
        //
        // Opening for real IS the probe. A poll on `SerialSource::openable`
        // here would open and immediately close, and on Linux `cdc_acm` a close
        // discards whatever the open's read URBs fetched — so the poll in front
        // of a reader consumes exactly the boot it is waiting for. A failed
        // open is already the "not back yet" signal.
        serial::open_when_back(&b.port, b.baud, board::RETURN_BUDGET)
            .map_err(|err| format!("board {} after the write: {err}", b.id))
    })?;

    // Now attached, so this boot IS observable — the same ordering the first
    // read relies on. Verifying through a reset we caused while listening is
    // what makes "the flash took" a fact rather than espflash's opinion.
    // Isolation is deliberately not propagated here: the verification below
    // compares the content hash, so a stale identity from the pre-flash boot
    // cannot pass as the new image — it fails as "flash did not take", loudly.
    let _ = board::reset_attached(board, l)?;

    // Verify the write TOOK. A flash that reported success and left the board
    // running something else must not pass silently — that is the failure a
    // stamp file can never detect, and the whole reason for asking the board.
    board::no_holes(l, "after flashing")?;
    match board::read_identity(l, banner, board::IDENTITY_BUDGET)? {
        Capture::Identified(now) if want.matched_by(&now) => Ok(Ensured::Flashed { why, now }),
        Capture::Identified(now) => Err(format!(
            "flash did not take: board reports {now}, image is {want}.\n\
             If the marks agree and only the hash differs, `espflash`'s app_elf_sha256 is not \
             sha256 of the ELF file, and this guard's comparison is unsound — do not paper over it."
        )),
        Capture::NoIdentity | Capture::NoApplication => Err(format!(
            "board {} did not name itself after flashing {} — the image may be bad",
            board.id,
            elf.display()
        )),
    }
}

/// A bounded wait for an arbitrary substring, for callers gating on bring-up.
///
/// # Errors
/// If the marker never appears, or the port dies.
pub fn await_marker<S: Source>(l: &mut Listener<S>, marker: &str, budget: Duration) -> Result<String, String> {
    match l.wait_for_line(marker, budget) {
        crate::listen::Outcome::Matched(line) => Ok(line),
        crate::listen::Outcome::DeadlineExpired => {
            Err(format!("never printed {marker:?} within {budget:?} of a reset — it did not come up"))
        }
        crate::listen::Outcome::SourceFailed(m) => Err(m),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(mark: &str, version: &str, sha: &str) -> Identity {
        Identity { mark: mark.into(), version: version.into(), sha256_prefix: sha.into() }
    }

    /// An `identity`-feature build: the descriptor's version field holds the
    /// git build mark, and `project_name` is the binary name.
    fn want_identity() -> ExpectedImage {
        ExpectedImage {
            version: "demos/display/a30fbf+".into(),
            project_name: "display".into(),
            sha256_prefix: "d862a888b3f7".into(),
        }
    }

    /// A plain build: the descriptor's version field is the crate version.
    /// Captured from a real CoreS3 boot line —
    /// `identity: display version=0.1.0 app_elf_sha256=515d5577fa8e`.
    fn want_plain() -> ExpectedImage {
        ExpectedImage { version: "0.1.0".into(), project_name: "display".into(), sha256_prefix: "515d5577fa8e".into() }
    }

    #[test]
    fn an_identity_build_matches_on_the_mark() {
        assert!(want_identity().matched_by(&ident("demos/display/a30fbf+", "0.1.0", "d862a888b3f7")));
    }

    /// THE case that failed on real hardware, twice, on two different chips
    /// (`m5stack-core#68`). `espflash` 4.x leaves `app_elf_sha256` as zeros in
    /// the image it writes, so the board reports zeros truthfully while this
    /// side computes `sha256` of the ELF file. Compared unconditionally, that
    /// is a guaranteed mismatch: every verification said "flash did not take"
    /// about an image that had just been written correctly.
    #[test]
    fn a_board_reporting_no_hash_matches_on_the_mark_alone() {
        assert!(want_identity().matched_by(&ident("demos/display/a30fbf+", "0.1.0", "000000000000")));
        assert!(want_plain().matched_by(&ident("display", "0.1.0", "000000000000")));
    }

    /// The mark still has to agree. A zero hash relaxes the hash check only —
    /// it must not turn into "anything matches".
    #[test]
    fn a_zero_hash_does_not_excuse_a_wrong_mark() {
        assert!(!want_identity().matched_by(&ident("demos/display/deadbee+", "0.1.0", "000000000000")));
    }

    /// If `espflash` ever populates the field again, the check tightens by
    /// itself — a nonzero hash that disagrees is still a mismatch.
    #[test]
    fn a_nonzero_hash_is_still_compared() {
        assert!(!want_identity().matched_by(&ident("demos/display/a30fbf+", "0.1.0", "ffffffffffff")));
    }

    /// THE case that failed on real hardware. Without the `identity` feature
    /// the board prints its BINARY NAME as the mark and the descriptor's
    /// version as `version=` — so a comparison that only ever looked at the
    /// descriptor's version field saw `display` vs `0.1.0`, declared a
    /// mismatch, and reflashed a board that was already correct. Every
    /// time.
    #[test]
    fn a_plain_build_matches_on_the_binary_name_and_version() {
        assert!(want_plain().matched_by(&ident("display", "0.1.0", "515d5577fa8e")));
    }

    /// THE case the hash exists for: same names, different uncommitted edits.
    #[test]
    fn one_mark_with_two_contents_does_not_match() {
        assert!(!want_identity().matched_by(&ident("demos/display/a30fbf+", "0.1.0", "0102030405ff")));
        assert!(!want_plain().matched_by(&ident("display", "0.1.0", "0102030405ff")));
    }

    /// The hash alone must not carry a match — it does not say which program.
    #[test]
    fn one_content_with_two_marks_does_not_match() {
        assert!(!want_identity().matched_by(&ident("demos/lvgl/a30fbf+", "0.1.0", "d862a888b3f7")));
        assert!(!want_plain().matched_by(&ident("lvgl", "0.1.0", "515d5577fa8e")));
    }

    /// In the plain shape the reported `version=` is checked too, so a match
    /// pins both descriptor fields rather than just the binary name.
    #[test]
    fn a_plain_build_with_the_wrong_version_does_not_match() {
        assert!(!want_plain().matched_by(&ident("display", "0.2.0", "515d5577fa8e")));
    }

    #[test]
    fn the_expected_image_names_both_halves_when_displayed() {
        let s = want_identity().to_string();
        assert!(s.contains("demos/display/a30fbf+"), "{s}");
        assert!(s.contains("d862a888b3f7"), "{s}");
    }

    /// Every field is **labelled**, so an `identity` build cannot render as the
    /// plain shape with its values shuffled — `secc version=oxichg/secc/…`
    /// reads as a corrupted value rather than as a mismatch.
    #[test]
    fn every_field_is_labelled_so_the_shape_cannot_be_misread() {
        let s = want_identity().to_string();
        assert!(s.starts_with("project_name="), "the leading field must say what it is: {s}");
        assert!(s.contains("version=demos/display/a30fbf+"), "the mark is the descriptor's version here: {s}");
        assert!(s.contains("app_elf_sha256=d862a888b3f7"), "{s}");
    }

    #[test]
    fn a_missing_elf_is_named_in_the_error() {
        let e = identity_of_elf(Path::new("/definitely/not/an/elf"), 12).expect_err("must fail");
        assert!(e.contains("/definitely/not/an/elf"), "the error must name the file: {e}");
    }

    /// A file with no descriptor is refused with an actionable message rather
    /// than yielding an empty mark that would mismatch forever.
    #[test]
    fn a_file_without_a_descriptor_is_refused_with_a_reason() {
        let p = std::env::temp_dir().join(format!("hil-noelf-{}", std::process::id()));
        std::fs::write(&p, b"not an elf, no magic word here").expect("write temp");
        let e = identity_of_elf(&p, 12).expect_err("must fail");
        let _ = std::fs::remove_file(&p);
        assert!(e.contains("app_desc!"), "must say what is missing: {e}");
    }

    #[test]
    fn the_cores3_default_pins_the_flash_frequency() {
        // espflash defaults to 40 MHz; code runs from flash by XIP, so a
        // measurement must not depend on which tool wrote the image.
        assert_eq!(FlashConfig::cores3().flash_freq.as_deref(), Some("80mhz"));
        assert_eq!(FlashConfig::cores3().chip, "esp32s3");
    }
}

/// The on-target tier for [`ensure_image`]: the decision that spends a flash.
///
/// # Prerequisite (`conventions/testing.md` §4)
///
/// `#[ignore]`d because it needs **the rig**, plus the image the board runs:
///
/// - `M5STACK_HIL_BOARD` — the board's MAC, or a full `/dev/serial/by-id` path;
/// - `M5STACK_HIL_IMAGE` — the ELF to ensure. Any image the board can run; the
///   first cycle writes it if it differs, which is what establishes the state
///   the rest of the test is about;
/// - optionally `M5STACK_HIL_BANNER`, and `M5STACK_HIL_SKIP_RUNS` (default 10).
///
/// ```sh
/// M5STACK_HIL_BOARD=1C:DB:D4:BA:83:38 M5STACK_HIL_IMAGE=target/…/app \
///   cargo test -- --ignored
/// ```
///
/// # What it catches that a host test cannot
///
/// A subtly wrong comparison does not fail — it **flashes**, correctly-looking,
/// on every run. That costs ~20 s and NOR endurance each time and is invisible
/// unless someone counts. The host suite proves `matched_by` against
/// constructed pairs; it cannot prove that the pair `ensure_image` builds from
/// a real board and a real ELF agrees.
#[cfg(test)]
mod ontarget {
    use std::path::PathBuf;

    use super::{Ensured, ensure_image};
    use crate::{
        board::{
            self, IDENTITY_BUDGET,
            ontarget::{banner, board},
        },
        listen::Listener,
        serial::DrainedSource,
    };

    /// Consecutive `ensure_image` runs on an unchanged image must all skip.
    ///
    /// Cycle 1 may write — that is how the state is established without
    /// requiring the caller to have flashed by hand first. Every cycle after it
    /// must skip, because nothing changed in between. One spurious flash is the
    /// whole defect: it means the comparison is wrong in a way that still
    /// produces a working board, so nothing else will ever complain.
    #[test]
    #[ignore = "needs the rig: M5STACK_HIL_BOARD=<MAC> M5STACK_HIL_IMAGE=<ELF>"]
    fn an_unchanged_image_is_never_reflashed() {
        let elf = PathBuf::from(
            std::env::var("M5STACK_HIL_IMAGE")
                .expect("M5STACK_HIL_IMAGE is unset — this tier needs the ELF the board runs"),
        );
        let (b, banner) = (board(), banner());
        let runs: usize = std::env::var("M5STACK_HIL_SKIP_RUNS").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
        let cfg = board::ontarget::flash_config();

        let mut flashed = 0;
        for i in 1..=runs {
            let port = DrainedSource::open(&b.port, b.baud).expect("open the board");
            let mut l = Listener::new(port);
            // A reset that could not be isolated makes the identity read
            // below suspect, and a suspect read is what spends a flash — so it
            // fails here rather than being discovered as a spurious write.
            let iso = board::reset_attached(&b, &mut l).unwrap_or_else(|e| panic!("run {i}: reset: {e}"));
            assert_eq!(iso, board::Isolation::Clean, "run {i}: reset not isolated; a stale line may be read");
            let cap = board::read_identity(&mut l, banner.as_deref(), IDENTITY_BUDGET)
                .unwrap_or_else(|e| panic!("run {i}: {e}"));

            // The board decides the comparison width, so read it back rather
            // than assuming: the ELF side truncates to match what the board
            // prints, never the other way round.
            let width = match &cap {
                board::Capture::Identified(id) => id.hash_prefix_len(),
                _ => 12,
            };
            match ensure_image(&mut l, &b, &cfg, &elf, &cap, banner.as_deref(), width)
                .unwrap_or_else(|e| panic!("run {i}: {e}"))
            {
                Ensured::AlreadyCurrent(_) => {}
                Ensured::Flashed { why, .. } => {
                    flashed += 1;
                    assert_eq!(i, 1, "run {i} of {runs} flashed an UNCHANGED image — {why}");
                }
            }
        }
        assert!(flashed <= 1, "{flashed} writes across {runs} runs on one image");
    }
}
