// SPDX-License-Identifier: MIT OR Apache-2.0
//! Addressing a board, resetting it, and reading what it says on the way up.
//!
//! Boundary: **the board as a thing that can be named, restarted and asked what
//! it is.** Deciding whether to flash it fails that sentence
//! ([`crate::flash`]), and so does judging a run ([`crate::gate`]).

use std::time::Duration;

use crate::{
    identity::{self, Identity},
    listen::{Listener, Outcome, Source},
    serial::DrainedSource,
    wait,
};

/// Ceiling, not an expectation: ~1 s on a healthy CoreS3. The wait returns as
/// soon as the port opens, so a generous ceiling is free.
pub const RETURN_BUDGET: Duration = Duration::from_secs(10);

/// How often the absent port is looked for while it is away. See
/// [`crate::wait`] for why this one condition is polled at all.
pub const RETURN_GAP: Duration = Duration::from_millis(50);

/// How long to look for the banner once the identity has not appeared. They are
/// adjacent boot lines, so this is generous.
pub const IDENTITY_GAP: Duration = Duration::from_secs(2);

/// Fail-loud ceiling, not a latency budget: the line goes out in the first
/// fraction of a second, so reaching this means none is coming.
pub const IDENTITY_BUDGET: Duration = Duration::from_secs(10);

/// How long the port must be silent before a reset for the boots either side of
/// it to be separable. Long enough to outlast USB latency and a log burst,
/// short enough to be free on a quiet board.
pub const QUIET_FOR: Duration = Duration::from_millis(150);

/// How long to keep trying to establish that silence before giving up and
/// saying so.
pub const QUIESCE_BUDGET: Duration = Duration::from_secs(3);

/// What `m5stack-core`'s console writes when its ring overran.
///
/// The ring is overwrite-**oldest**, so an overrun destroys the earliest output
/// — the identity. Written straight to TX, bypassing the ring, so it cannot
/// itself be lost. Duplicated from `m5stack_core::io::console`, which is
/// `no_std` and Xtensa-only.
pub const DROP_MARKER: &str = "[CONSOLE-DROP";

/// Espressif's USB-Serial-JTAG `VID:PID`, which `probe-rs` prefixes to a
/// probe's serial number: `303a:1001:<MAC>`.
const ESP_JTAG_VID_PID: &str = "303a:1001";

/// The oldest `probe-rs` whose CLI contract this relies on (`reset --chip
/// --non-interactive --probe`). A subprocess cannot be pinned in `Cargo.toml`.
///
/// Checked because failure is otherwise **silent**: an older CLI hangs on an
/// interactive probe picker, or resets whichever board it chose.
pub const PROBE_RS_MIN: (u32, u32) = (0, 32);

/// The `major.minor` from a `probe-rs --version` banner, e.g.
/// `probe-rs 0.32.0 (git commit: crates.io)`.
fn parse_probe_rs_version(banner: &str) -> Option<(u32, u32)> {
    let ver = banner.split_whitespace().find(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = ver.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// Verify the installed `probe-rs` is new enough, once per process.
///
/// # Errors
/// If `probe-rs` cannot be run, its version cannot be read, or it is older than
/// [`PROBE_RS_MIN`].
fn check_probe_rs() -> Result<(), String> {
    static CHECKED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    CHECKED
        .get_or_init(|| {
            let out = std::process::Command::new("probe-rs")
                .arg("--version")
                .output()
                .map_err(|e| format!("cannot run probe-rs: {e} — is it installed?"))?;
            let banner = String::from_utf8_lossy(&out.stdout);
            let got = parse_probe_rs_version(&banner)
                .ok_or_else(|| format!("could not read a version out of `probe-rs --version`: {banner:?}"))?;
            if got < PROBE_RS_MIN {
                return Err(format!(
                    "probe-rs {}.{} is too old: this needs at least {}.{} for `--non-interactive` \
                     and `--probe` selection.\nWithout them a reset can sit on an interactive \
                     picker, or restart the wrong board on a rig with several probes.",
                    got.0, got.1, PROBE_RS_MIN.0, PROBE_RS_MIN.1
                ));
            }
            Ok(())
        })
        .clone()
}

/// How a board is restarted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reset {
    /// Over JTAG, through the `probe-rs` CLI — the CoreS3 default. Goes through
    /// the chip's debug unit, so it does not depend on the serial adapter
    /// honouring RTS/DTR. A subprocess because the `probe-rs` *library* carries
    /// no Espressif targets (see `Cargo.toml`); version-pinned by
    /// [`PROBE_RS_MIN`].
    ProbeRs {
        chip: String,
        /// `VID:PID:Serial`. **Load-bearing on a rig with more than one probe**:
        /// unqualified, `probe-rs reset` prompts or picks, so it can restart
        /// somebody else's board. Derived from the MAC for a CoreS3.
        probe: Option<String>,
    },
    /// Over the serial port's control lines, through `espflash`. For a board
    /// with no debug probe — a Fire27 behind its USB-serial bridge.
    ///
    /// Worth keeping as the **recovery** route on a board that normally resets
    /// over JTAG: driving the tty's control lines is independent of the debug
    /// unit, so it still works when that does not — probe-rs 0.30 times out
    /// resetting an Xtensa target while its probe enumeration is fine.
    ///
    /// That independence is only against the debug unit. `esptool` does the
    /// same job by the same means where it is installed.
    Espflash,
}

/// A board, addressed by something stable.
#[derive(Debug, Clone)]
pub struct Board {
    /// The stable identity used for the lock — a MAC or an adapter serial,
    /// **never** a `ttyACM` index, which renumbers on replug.
    pub id: String,
    /// The device path. Kept whole rather than re-derived, because it does not
    /// change across a reset: only its availability does.
    pub port: String,
    pub baud: u32,
    pub reset: Reset,
}

impl Board {
    /// A CoreS3 over its native USB-Serial-JTAG, addressed by MAC.
    ///
    /// udev maintains this symlink, so it is **stable by construction**: it
    /// survives the re-enumeration a reset causes and names the same board
    /// whatever `ttyACM` number the kernel hands out this time. That removes a
    /// class of defect rather than guarding against it — there is nothing to
    /// re-resolve after a reset and therefore nothing to forget.
    #[must_use]
    pub fn cores3(mac: &str) -> Self {
        Self {
            id: mac.to_string(),
            port: format!("/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_{mac}-if00"),
            baud: 1_000_000,
            reset: Reset::ProbeRs {
                chip: "esp32s3".into(),
                // The USB-Serial-JTAG's probe serial is the MAC, and Espressif's
                // VID:PID is fixed — so the probe can be named exactly without
                // asking the config for something it already knows.
                probe: Some(format!("{ESP_JTAG_VID_PID}:{mac}")),
            },
        }
    }

    /// A board at an explicit port — for anything whose `by-id` name this crate
    /// does not construct (a Fire27 behind its USB-serial bridge, a bench
    /// adapter). `id` must still be stable; the lock refuses a tty path.
    /// A board with no debug probe resets over its serial control lines, so
    /// this defaults to [`Reset::Espflash`]. Override `reset` for a board that
    /// has a probe but a non-derivable port name.
    #[must_use]
    pub fn at_port(id: &str, port: &str, baud: u32) -> Self {
        Self { id: id.to_string(), port: port.to_string(), baud, reset: Reset::Espflash }
    }
}

/// Restart the chip, by whichever route [`Board::reset`] names.
///
/// A **hardware** reset, not a console command: the latter needs firmware alive
/// to read it, which is exactly what a recovery path cannot assume. Not
/// `esptool` either — it is a Python module and is often absent where the Rust
/// tooling is installed.
///
/// The caller must not hold the port for [`Reset::Espflash`], which opens it
/// exclusively; [`Listener::across_reset`] enforces that.
///
/// **No route falls back to another.** A board resets the way it is configured
/// to, or the run fails saying so. Quietly retrying a failed JTAG reset over
/// the control lines would hide a dead probe behind a board that still comes
/// up, which is the kind of "works more often now" that costs an afternoon
/// later. Prefer JTAG wherever a probe exists — that is a default, not a
/// fallback.
///
/// # Errors
/// If the reset tool cannot be run, or reports failure.
pub fn hard_reset(board: &Board) -> Result<(), String> {
    let (prog, args) = match &board.reset {
        Reset::ProbeRs { chip, probe } => {
            check_probe_rs()?;
            let mut a: Vec<String> = vec!["reset".into(), "--chip".into(), chip.clone(), "--non-interactive".into()];
            // Without this, a rig with two probes gets an interactive picker or
            // an arbitrary choice — either of which can reset the wrong board.
            if let Some(p) = probe {
                a.push("--probe".into());
                a.push(p.clone());
            }
            ("probe-rs", a)
        }
        Reset::Espflash => (
            "espflash",
            vec![
                "reset".into(),
                "--port".into(),
                board.port.clone(),
                "--after".into(),
                "hard-reset".into(),
                "--non-interactive".into(),
            ],
        ),
    };
    let out = std::process::Command::new(prog)
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run {prog}: {e} — is it installed?"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{prog} could not reset board {}: {}", board.id, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Reset `board` while a listener stays attached, where the route allows it.
///
/// The ESP32-S3's USB-Serial-JTAG **discards output when no host is reading**,
/// and the identity goes out at ~0.3 s of uptime — so resetting and *then*
/// opening misses it (measured: 0 bytes from a board that was printing).
///
/// [`Reset::ProbeRs`] removes that rather than racing it: a JTAG reset does not
/// re-enumerate the USB device, so the fd survives it. [`Reset::Espflash`]
/// cannot hold the port and inherits the race — a property of the route, and
/// the reason probe-rs is preferred wherever a probe exists.
///
/// # Errors
/// If the reset fails, or (on the espflash path) the board never comes back.
pub fn reset_attached(board: &Board, l: &mut Listener<DrainedSource>) -> Result<Isolation, String> {
    // Silence FIRST, then the barrier, then the reset. The order is the whole
    // guarantee: a barrier drawn without silence still lets in-flight bytes
    // from the old boot land behind it, and one drawn *after* the reset would
    // discard the identity this reset exists to read.
    let isolation = if l.quiesce(QUIET_FOR, QUIESCE_BUDGET) { Isolation::Clean } else { Isolation::Chatty };
    l.discard_backlog();
    match &board.reset {
        // The port is NOT released: that is the whole point.
        Reset::ProbeRs { .. } => hard_reset(board),
        Reset::Espflash => l.across_reset(|| reset_and_reopen(board)),
    }?;
    Ok(isolation)
}

/// Reset `board` and come back with an open port on the other side of it.
///
/// **Not how a driver starts.** Open first, attach a [`Listener`], and reset
/// through [`reset_attached`] — on a route that never releases the port there
/// is no re-enumeration to attach at, so resetting first loses the whole boot.
/// This is for the `espflash` route, which must let go, and for re-attaching
/// after a flash.
///
/// What a blind `sleep 2` stands in for, each step able to fail by name. The
/// wait is needed whichever route reset the board: the ESP32-S3's
/// USB-Serial-JTAG resets *with the chip*, so the endpoint disappears even for
/// a JTAG reset that never touched the tty.
///
/// **Opening for real is the probe.** A throwaway
/// [`crate::serial::SerialSource::openable`] poll would discard what its own
/// read URBs fetched — the boot being waited for — and a failed open is
/// already the "not back yet" signal.
///
/// # Errors
/// If the reset fails, or the device never comes back.
pub fn reset_and_reopen(board: &Board) -> Result<DrainedSource, String> {
    hard_reset(board)?;
    // The PATH does not change across a reset — only its availability does — so
    // this waits for the device to come back rather than for a new name.
    wait::until(&format!("board {} to re-enumerate", board.id), RETURN_BUDGET, RETURN_GAP, || {
        DrainedSource::open(&board.port, board.baud).map_err(|e| format!("{}: {e}", board.port))
    })
}

/// Whether the boot a reset caused can be told apart from the one before it.
///
/// `#[must_use]` because ignoring it is the bug: a caller that drops this has
/// silently accepted that its waits may match the previous boot.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    /// The port fell silent before the reset, so nothing from the previous boot
    /// is buffered or in flight: every later match is from this boot.
    Clean,
    /// The board never stopped talking, so a wait may still match output from
    /// before the reset. Harmless for an identity (a stale one is caught by its
    /// hash) but **not** for `--until`, where it turns a miss into a false pass.
    Chatty,
}

/// What a capture of a board's boot told us about the image on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capture {
    /// The board booted and named itself.
    Identified(Identity),
    /// The board booted but printed no usable identity — an image built without
    /// `app_desc!`, or one predating the identity line. A caller **may** act on
    /// this: flash it.
    NoIdentity,
    /// The banner never appeared, so the board never reached the application. A
    /// caller may **not** treat this as "old image": nothing was observed, and
    /// a bad image, a board held in the bootloader and a broken capture all
    /// look like this.
    ///
    /// Only produced when a banner was supplied — see [`read_identity`].
    NoApplication,
}

/// Read the identity a board prints **at boot**, after a reset the caller
/// controls.
///
/// The reset has to be ours: the line goes out in the first fraction of a
/// second, so the only way to be listening is to have caused the restart. A
/// board that booted an hour ago cannot be asked this way.
///
/// `banner` is a substring the application prints, used **only** to tell "never
/// reached the application" apart from "this image carries no identity". With
/// `None` the two collapse into [`Capture::NoIdentity`] rather than being
/// guessed at.
///
/// **The identity is waited for first**, because `console::install` prints it
/// *before* the application prints anything — so the banner is the later line.
/// Waiting on the banner first would advance
/// [`Listener::wait_for_line`]'s cursor past the identity, which could then
/// never be found. The banner fallback stays exact because the cursor only
/// advances on a match.
///
/// # Errors
/// Only a dead *port* is an error, because waiting longer cannot help that. A
/// missing identity is a [`Capture`], not a failure.
pub fn read_identity<S: Source>(
    l: &mut Listener<S>,
    banner: Option<&str>,
    budget: Duration,
) -> Result<Capture, String> {
    match l.wait_for_line(identity::MARKER, budget) {
        Outcome::Matched(line) => Ok(identity::from_line(&line).map_or(Capture::NoIdentity, Capture::Identified)),
        // No identity. A banner is what distinguishes "booted, but this image
        // cannot say what it is" from "never got as far as the application".
        Outcome::DeadlineExpired => match banner {
            None => Ok(Capture::NoIdentity),
            Some(b) => match l.wait_for_line(b, IDENTITY_GAP) {
                Outcome::Matched(_) => Ok(Capture::NoIdentity),
                Outcome::DeadlineExpired => Ok(Capture::NoApplication),
                Outcome::SourceFailed(m) => Err(m),
            },
        },
        Outcome::SourceFailed(m) => Err(m),
    }
}

/// Did the board's console ring overrun during this capture?
///
/// The ring is overwrite-**oldest**, so an overrun destroys the earliest output
/// — the identity line — which looks exactly like an image that carries no
/// identity at all. The firmware marks the loss on a path that cannot itself be
/// lost, so this is evidence rather than inference. Returns the marker line so
/// a caller can quote it.
#[must_use]
pub fn console_hole<S: Source>(l: &Listener<S>) -> Option<String> {
    let hay = String::from_utf8_lossy(l.bytes());
    let at = hay.find(DROP_MARKER)?;
    Some(hay[at..].lines().next().unwrap_or(DROP_MARKER).trim().to_string())
}

/// The same fact as a hard error, for callers that only ever *verify*.
///
/// A flash guard deliberately does **not** use this: a capture it cannot trust
/// is a reason to write the image, not a reason to stop.
///
/// # Errors
/// If the capture has a hole, with the marker quoted.
pub fn no_holes<S: Source>(l: &Listener<S>, what: &str) -> Result<(), String> {
    match console_hole(l) {
        None => Ok(()),
        Some(line) => Err(format!(
            "{what}: the board's console ring overran and discarded output — {line}\n\
             The ring overwrites the OLDEST bytes, so the earliest lines (the identity) are the \
             ones lost. Nothing may be concluded from this capture."
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, io};

    use super::*;

    struct Scripted(VecDeque<Vec<u8>>);

    impl Scripted {
        fn new(chunks: &[&str]) -> Self {
            Self(chunks.iter().map(|c| c.as_bytes().to_vec()).collect())
        }
    }

    impl Source for Scripted {
        fn read_available(&mut self, budget: Duration) -> io::Result<Vec<u8>> {
            let Some(c) = self.0.pop_front() else {
                // Out of script: behave like a silent board — block for the
                // budget rather than returning instantly, so a deadline test
                // measures a deadline and not a busy loop.
                std::thread::sleep(budget.min(Duration::from_millis(40)));
                return Ok(Vec::new());
            };
            Ok(c)
        }
    }

    const SHORT: Duration = Duration::from_millis(150);
    const BANNER: &str = "m5stack-core demo";
    const ID_LINE: &str =
        "[00000.276 INFO ] identity: demos/display/a30fbf+ version=0.1.0 app_elf_sha256=d862a888b3f7\n";

    /// A real boot: `console::install` prints the identity, then the
    /// application prints its banner. The fixture used to have these the other
    /// way round, which is why the inverted wait in `read_identity` passed its
    /// own tests while missing every identity on hardware.
    fn boot(extra: &str) -> String {
        format!("{ID_LINE}[00000.280 INFO ] {BANNER}\n{extra}")
    }

    /// Boot output for an image with no identity at all.
    fn boot_without_identity(extra: &str) -> String {
        format!("[00000.280 INFO ] {BANNER}\n{extra}")
    }

    #[test]
    fn a_board_that_names_itself_yields_its_identity() {
        let mut l = Listener::new(Scripted::new(&[&boot("")]));
        match read_identity(&mut l, Some(BANNER), SHORT) {
            Ok(Capture::Identified(id)) => {
                assert_eq!(id.mark, "demos/display/a30fbf+");
                assert_eq!(id.sha256_prefix, "d862a888b3f7");
            }
            other => panic!("expected an identity, got {other:?}"),
        }
    }

    /// The identity is found with no banner supplied — the common case for a
    /// BSP-only image, where `console::install`'s line is the first thing said.
    #[test]
    fn no_banner_still_finds_the_identity() {
        let mut l = Listener::new(Scripted::new(&[ID_LINE]));
        assert!(matches!(read_identity(&mut l, None, SHORT), Ok(Capture::Identified(_))));
    }

    /// An image built without `app_desc!`: it booted, it just cannot say what
    /// it is. A caller MAY act on this — flash it.
    #[test]
    fn a_booted_board_with_no_identity_is_not_the_same_as_a_dead_one() {
        let mut l = Listener::new(Scripted::new(&[&boot_without_identity("[00000.277 INFO ] wifi: associated\n")]));
        assert_eq!(read_identity(&mut l, Some(BANNER), SHORT), Ok(Capture::NoIdentity));
    }

    /// THE distinction a banner buys. Bootloader chatter and then nothing: the
    /// board never reached the application, and reporting that as "no identity"
    /// would spend a 40 s flash on a diagnosis nobody made.
    #[test]
    fn a_board_that_never_reached_the_application_is_reported_as_such() {
        let mut l = Listener::new(Scripted::new(&[
            "I (175) esp_image: segment 1: paddr=00088540\n",
            "I (377) boot: Loaded app from partition at offset 0x10000\n",
        ]));
        assert_eq!(read_identity(&mut l, Some(BANNER), SHORT), Ok(Capture::NoApplication));
    }

    /// Without a banner that distinction is not available, and this crate says
    /// so by collapsing to `NoIdentity` rather than guessing which it was.
    #[test]
    fn without_a_banner_silence_is_reported_as_no_identity_not_inferred() {
        let mut l = Listener::new(Scripted::new(&[]));
        assert_eq!(read_identity(&mut l, None, SHORT), Ok(Capture::NoIdentity));
    }

    #[test]
    fn total_silence_with_a_banner_is_no_application() {
        let mut l = Listener::new(Scripted::new(&[]));
        assert_eq!(read_identity(&mut l, Some(BANNER), SHORT), Ok(Capture::NoApplication));
    }

    /// A half-parsed identity is not an identity — the rule
    /// `identity::from_line` enforces, checked here through the capture
    /// path that actually uses it.
    #[test]
    fn a_line_missing_the_hash_does_not_count_as_identified() {
        let mut l = Listener::new(Scripted::new(&[&boot_without_identity(
            "[00000.276 INFO ] identity: demos/display/a30fbf+ version=0.1.0\n",
        )]));
        assert_eq!(read_identity(&mut l, Some(BANNER), SHORT), Ok(Capture::NoIdentity));
    }

    /// The ring overwrites the OLDEST bytes, so an overrun looks exactly like
    /// an image with no identity — and would be "fixed" by a needless
    /// flash. The marker cannot itself be lost, so its presence must veto
    /// every conclusion.
    #[test]
    fn a_capture_with_a_hole_refuses_to_support_a_conclusion() {
        let mut l = Listener::new(Scripted::new(&[&format!("{DROP_MARKER} 512B]\n"), &boot("")]));
        let _ = read_identity(&mut l, Some(BANNER), SHORT);
        let e = no_holes(&l, "board X").expect_err("a hole must veto");
        assert!(e.contains("overran"), "{e}");
        assert!(e.contains("512B"), "must quote the marker so the size is visible: {e}");
        assert!(e.contains("OLDEST"), "must say which end was lost: {e}");
    }

    #[test]
    fn a_clean_capture_passes_the_hole_check() {
        let mut l = Listener::new(Scripted::new(&[&boot(ID_LINE)]));
        let _ = read_identity(&mut l, Some(BANNER), SHORT);
        assert_eq!(no_holes(&l, "board X"), Ok(()));
    }

    #[test]
    fn the_probe_rs_version_banner_is_parsed() {
        assert_eq!(parse_probe_rs_version("probe-rs 0.32.0 (git commit: crates.io)"), Some((0, 32)));
        assert_eq!(parse_probe_rs_version("probe-rs 1.4.7"), Some((1, 4)));
    }

    /// A banner this cannot read must be an error, not a silent pass — the
    /// whole point of the check is that the failure it prevents is quiet.
    #[test]
    fn an_unreadable_version_banner_is_none_not_a_guess() {
        assert_eq!(parse_probe_rs_version(""), None);
        assert_eq!(parse_probe_rs_version("probe-rs unknown"), None);
        assert_eq!(parse_probe_rs_version("probe-rs 0"), None);
    }

    /// The comparison is on `(major, minor)` as a tuple, so 0.9 is correctly
    /// older than 0.32 — the case a string compare gets wrong.
    #[test]
    fn version_ordering_is_numeric_not_lexicographic() {
        assert!(parse_probe_rs_version("probe-rs 0.9.0").expect("parses") < PROBE_RS_MIN);
        assert!(parse_probe_rs_version("probe-rs 0.32.0").expect("parses") >= PROBE_RS_MIN);
        assert!(parse_probe_rs_version("probe-rs 1.0.0").expect("parses") >= PROBE_RS_MIN);
    }

    #[test]
    fn a_cores3_is_addressed_by_a_stable_by_id_symlink_never_a_tty_index() {
        let b = Board::cores3("1C:DB:D4:BA:83:38");
        assert!(b.port.starts_with("/dev/serial/by-id/"), "{}", b.port);
        assert!(b.port.contains("1C:DB:D4:BA:83:38"));
        assert!(!b.port.contains("ttyACM"), "a tty index renumbers on replug: {}", b.port);
        assert_eq!(b.id, "1C:DB:D4:BA:83:38", "the lock key must be the stable identity");
    }
}
