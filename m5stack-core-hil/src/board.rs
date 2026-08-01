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
    serial::{ControlLines, ControlPort, DrainedSource, LineControl},
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

/// How long `EN` is held low by [`reset_lines_sequence`].
///
/// `esptool`'s own hard reset holds it for 100 ms, and this matches it rather
/// than shortening it: the value is the one every ESP32 board has been reset
/// with for years, so a board that needs longer is a board with a fault worth
/// finding, not a constant worth nudging.
pub const EN_LOW_HOLD: Duration = Duration::from_millis(100);

/// How long the control lines are left idle before `EN` is pulled.
///
/// **Not a race workaround** — a settling time for real hardware. The kernel
/// raises `DTR` and `RTS` when a tty is opened, and `EN` is pulled up through
/// an RC network (order of a millisecond on the usual `10 kΩ`/`100 nF`). Driving
/// the lines idle and pulling `EN` in the same instant would start the reset
/// pulse from an indeterminate level rather than from a known-high one, so the
/// board's *own* charge curve, not this harness, sets how long that takes.
pub const LINE_SETTLE: Duration = Duration::from_millis(20);

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
    /// Over the serial port's `RTS`/`DTR` lines, driven **in this process** on
    /// the descriptor already held. The default for a board with no debug probe.
    ///
    /// `espflash reset` pulses the same lines, but as a subprocess it needs the
    /// tty to itself — so [`Reset::Espflash`] must release the port and re-open
    /// into a boot already under way, and the kernel submits no read URBs while
    /// it is closed. That window is exactly the boot. On a CoreS3 releasing is
    /// unavoidable; on a Fire27 it is not, because the USB-serial bridge is a
    /// **separate chip from the ESP32** and does not reset with it, so the fd
    /// stays valid throughout.
    ///
    /// Assumes the standard `esptool` auto-reset circuit (`RTS`→`EN`,
    /// `DTR`→`IO0`, mutually cancelling). If a board lacks it the pulse does
    /// nothing and the board keeps running, which surfaces as "never reached
    /// the application" rather than as a silent pass; `reset = "espflash"` is
    /// the escape hatch.
    SerialLines,
    /// Over the serial port's control lines, through `espflash`.
    ///
    /// Worth keeping as the **recovery** route on a board that normally resets
    /// over JTAG: driving the tty's control lines is independent of the debug
    /// unit, so it still works when that does not — probe-rs 0.30 times out
    /// resetting an Xtensa target while its probe enumeration is fine.
    ///
    /// That independence is against the *debug unit* only. It is **not** a
    /// second path to a [`Reset::SerialLines`] board: those are the same two
    /// lines driven from a subprocess instead of in-process, which is also why
    /// this one alone inherits the attach-after-reset race.
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
    ///
    /// The port is **not** derived from an adapter serial the way
    /// [`Board::cores3`] derives one from a MAC, and that is deliberate: a
    /// CoreS3's bridge is on the die and its `by-id` name has exactly one
    /// shape, whereas M5Stack has shipped ESP32 boards behind more than one
    /// USB-serial chip — a 1a86 (CH-series) on this bench, a CP2104 elsewhere —
    /// which produce different names for identical hardware.
    /// Guessing which would be a rule that is right until it is silently wrong,
    /// so the port is stated.
    ///
    /// A board with no debug probe resets over its serial control lines, so
    /// this defaults to [`Reset::SerialLines`] — driven from the held
    /// descriptor, not by releasing the port to `espflash`. Override `reset`
    /// for a board that has a probe but a non-derivable port name.
    #[must_use]
    pub fn at_port(id: &str, port: &str, baud: u32) -> Self {
        Self { id: id.to_string(), port: port.to_string(), baud, reset: Reset::SerialLines }
    }
}

/// Pulse `EN` low and release it, leaving `IO0` high so the chip boots the
/// **application** rather than the ROM downloader.
///
/// Split out from [`hard_reset`] and generic over [`LineControl`] so the
/// sequence can be proven on the host: the ioctl either works or returns
/// `EINVAL`, but the *order* is the part that can be plausibly wrong in a way
/// that costs a bench session — assert `DTR` at the wrong moment and the board
/// comes up in download mode, silent, looking exactly like an image that does
/// not boot.
///
/// # Errors
/// If a line cannot be driven — the descriptor is not a tty, typically.
pub fn reset_lines_sequence<T: LineControl>(lines: &T, id: &str) -> Result<(), String> {
    let step = |want: ControlLines, what: &str| {
        lines.set_control_lines(want).map_err(|e| {
            format!(
                "board {id}: cannot {what}: {e}\n\
                 This resets the board by pulsing the tty's RTS line. If the port is not a real \
                 serial device, set `reset = \"espflash\"` for this board in hil.toml."
            )
        })
    };
    // A known-idle start: the kernel raises both lines when the port is opened,
    // and on the cancelling circuit that leaves EN high but says nothing about
    // how long it has been there.
    step(ControlLines::IDLE, "release the reset lines")?;
    std::thread::sleep(LINE_SETTLE);
    step(ControlLines::RESET, "pull EN low")?;
    std::thread::sleep(EN_LOW_HOLD);
    step(ControlLines::IDLE, "release EN")?;
    Ok(())
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
        // No subprocess at all: open a control-only handle and pulse the lines.
        // This is the detached form; `reset_attached` uses the port it is
        // already holding instead, which is the one worth having.
        Reset::SerialLines => {
            let lines = ControlPort::open(&board.port).map_err(|e| format!("board {}: {e}", board.id))?;
            return reset_lines_sequence(&lines, &board.id);
        }
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
/// re-enumerate the USB device, so the fd survives it. [`Reset::SerialLines`]
/// removes it a second way for a board with no probe — the lines are pulsed on
/// the descriptor already held, and an external bridge does not reset with the
/// ESP32. [`Reset::Espflash`] is the only route that must let go, and inherits
/// the race; hence the other two are the defaults wherever they apply.
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
        Reset::SerialLines => {
            // Held, so the lines are driven through the capture rather than
            // around it. `None` means a previous reconnect failed, which is a
            // hard error and not something to reset around.
            let held = l.source_mut().ok_or_else(|| {
                format!("board {}: the listener has no port — an earlier reconnect failed", board.id)
            })?;
            reset_lines_sequence(held, &board.id)
        }
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
/// already the "not back yet" signal. A board behind an external bridge never
/// leaves at all, so the wait simply returns at once.
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

    /// Records every state the lines were driven to, in order. The reset
    /// sequence is pure ordering over this trait, so it is fully testable
    /// without a board — which matters, because every mistake it can make is
    /// one that looks like broken firmware from the outside.
    struct Recorder(std::cell::RefCell<Vec<ControlLines>>);

    impl Recorder {
        fn new() -> Self {
            Self(std::cell::RefCell::new(Vec::new()))
        }
        fn seen(&self) -> Vec<ControlLines> {
            self.0.borrow().clone()
        }
    }

    impl LineControl for Recorder {
        fn set_control_lines(&self, want: ControlLines) -> std::io::Result<()> {
            self.0.borrow_mut().push(want);
            Ok(())
        }
    }

    /// A board with no probe still gets a real reset: `EN` goes low and comes
    /// back up, from a known-idle start.
    #[test]
    fn the_serial_line_reset_pulses_en_low_and_releases_it() {
        let r = Recorder::new();
        reset_lines_sequence(&r, "fire27").expect("driving a recorder cannot fail");
        assert_eq!(
            r.seen(),
            vec![ControlLines::IDLE, ControlLines::RESET, ControlLines::IDLE],
            "must start idle, pull EN low, then release it"
        );
    }

    /// THE property worth a test. `DTR` drives `IO0`, and a chip released from
    /// reset with `IO0` low comes up in the ROM **download mode**: silent, and
    /// indistinguishable from an image that fails to boot. Asserting it at any
    /// point in the sequence would do that, so it is asserted at no point.
    #[test]
    fn the_reset_never_asserts_dtr_which_would_boot_into_download_mode() {
        let r = Recorder::new();
        reset_lines_sequence(&r, "fire27").expect("recorder");
        assert!(
            r.seen().iter().all(|l| !l.dtr),
            "DTR pulls IO0 low; the board would come up in the ROM downloader, not the app: {:?}",
            r.seen()
        );
    }

    /// The pulse must END with the chip running. A sequence that left `EN` held
    /// low would look exactly like a dead board to everything downstream.
    #[test]
    fn the_reset_leaves_both_lines_released() {
        let r = Recorder::new();
        reset_lines_sequence(&r, "fire27").expect("recorder");
        assert_eq!(r.seen().last().copied(), Some(ControlLines::IDLE), "the chip must be left running");
    }

    /// `EN` is held low for a real, board-sized interval rather than a
    /// same-instant toggle the hardware would never see.
    #[test]
    fn en_is_held_low_long_enough_for_the_chip_to_see_it() {
        let t0 = std::time::Instant::now();
        reset_lines_sequence(&Recorder::new(), "fire27").expect("recorder");
        assert!(t0.elapsed() >= EN_LOW_HOLD, "EN must be held for {EN_LOW_HOLD:?}, took {:?}", t0.elapsed());
    }

    /// A failure to drive the lines must name the board and offer the way out,
    /// because the commonest cause is a `port` that is not a serial device.
    #[test]
    fn a_line_that_cannot_be_driven_reports_the_escape_hatch() {
        struct Broken;
        impl LineControl for Broken {
            fn set_control_lines(&self, _: ControlLines) -> std::io::Result<()> {
                Err(std::io::Error::from_raw_os_error(25)) // ENOTTY
            }
        }
        let e = reset_lines_sequence(&Broken, "fire27").expect_err("must fail");
        assert!(e.contains("fire27"), "must name the board: {e}");
        assert!(e.contains("espflash"), "must offer the fallback route: {e}");
    }

    /// A probe-less board resets over its own control lines, held — not by
    /// handing the port to `espflash`, which would have to release it.
    #[test]
    fn a_port_addressed_board_defaults_to_the_held_line_reset() {
        assert_eq!(Board::at_port("fire27-586", "/dev/serial/by-id/x", 1_000_000).reset, Reset::SerialLines);
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
