// SPDX-License-Identifier: MIT OR Apache-2.0
//! The real [`crate::listen::Source`]: a serial port, owned for the run and
//! released by `Drop`.
//!
//! Boundary: **getting bytes off a tty.** Deciding what they mean fails that
//! sentence ([`crate::report`]), and so does deciding when to stop waiting
//! ([`crate::listen`]).
//!
//! `conventions/testing.md` §8.1 wants release on every exit path including
//! panic. Here the descriptor is owned by a value, so that is structural rather
//! than remembered — the entire argument for the harness being in Rust. An
//! orphaned reader holding a tty, so the next run reports "Device or resource
//! busy" while a live board looks dead, is this repo's most expensive tooling
//! failure.
//!
//! The read timeout is the kernel's: termios `VMIN = 0`, `VTIME = t`. `VTIME`
//! is fixed at configure time and in deciseconds, so it is set small once and
//! [`SerialSource::read_available`] loops until the caller's budget is spent.
//! That loop is not the poll §1 bans — the thread is blocked *in the kernel on
//! the port*, and a byte returns it immediately; `t` governs only how often we
//! wake while the board is **silent**.
//!
//! Termios is set with `stty` rather than a serial crate: it is already a hard
//! requirement of every script here, and adopting a second port abstraction
//! with its own idea of a baud rate costs more than it saves.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::FileTypeExt,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::listen::Source;

/// How long a silent read blocks before returning empty, in deciseconds
/// (termios `VTIME`). 1 = 100 ms: frequent enough that a deadline is honoured
/// promptly, rare enough to be free. Only reached when the board says nothing.
const VTIME_DECISECONDS: u8 = 1;

// Pinned at compile time, not in a test: `VTIME` is a `u8` of DECIseconds and
// termios truncates silently, so a later "make it 2 seconds" edit writing `20`
// would wrap to 2 s — or `200` to nothing — and quietly change the heartbeat
// with every test still green. A build error is the right place to find that.
// (A runtime `assert!` on a `const` is a tautology, which clippy says plainly.)
const _: () = assert!(VTIME_DECISECONDS >= 1, "0 means a blocking read with no timeout at all");
const _: () = assert!(VTIME_DECISECONDS <= 10, "beyond ~1 s a deadline stops being honoured promptly");

/// An owned serial port. Closes on drop, on every path.
#[derive(Debug)]
pub struct SerialSource {
    port: File,
    path: String,
}

impl SerialSource {
    /// Open `path` raw at `baud` and configure a kernel read timeout.
    ///
    /// # Errors
    /// If `stty` cannot configure the port, or the port cannot be opened.
    /// Both are terminal for a run: a port that cannot be configured cannot be
    /// trusted to deliver bytes intact.
    pub fn open(path: &str, baud: u32) -> io::Result<Self> {
        // Refuse if ANYTHING else holds this port: two readers split the byte
        // stream and both see holes. `BoardLock` cannot see a stray `cat`, and
        // `std` has no `flock`, so this is §8.1's other half — detect the
        // holder and refuse, naming it, never reclaiming it.
        if let Some((pid, cmd)) = Self::holder(path) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!(
                    "{path} is already open by pid {pid} ({cmd}).\n\
                     Two readers split the byte stream and both see holes, so this refuses \
                     rather than corrupting the run.\n\
                     NOT killing it — it may be another run or your own capture. To free it: kill {pid}"
                ),
            ));
        }
        // OPEN FIRST, CONFIGURE SECOND. Configuring via a descriptor `stty`
        // then closes leaves a window in which the discipline can lapse to the
        // driver's defaults — cooked mode with **echo on**, which types the
        // board's own log back at it and can issue console commands nobody
        // typed. The window is widest right after a reset, which is when the
        // identity is in flight. Holding the descriptor across the
        // configuration removes it.
        //
        // `io::Error`'s Display is bare, so the path is added: a failure that
        // does not name the port sends the reader to the wrong board.
        let port = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| io::Error::new(e.kind(), format!("{path}: {e}")))?;
        Self::configure(path, baud)?;
        Ok(Self { port, path: path.to_string() })
    }

    /// Put the port in the exact discipline a capture needs.
    ///
    /// Without `clocal` a read can block or hang up on carrier. Without
    /// `-hupcl`, **closing** the port lowers DTR — the line `esptool` pulses to
    /// reset this chip — so an ordinary close could reboot the board just
    /// measured. `-crtscts` disables flow control the CDC device lacks.
    fn configure(path: &str, baud: u32) -> io::Result<()> {
        let out = Command::new("stty")
            .args([
                "-F",
                path,
                &baud.to_string(),
                "raw",
                "-echo",
                "-echoe",
                "-echok",
                "-echoctl",
                "-echoke",
                "clocal",
                "-hupcl",
                "-crtscts",
                "min",
                "0",
                "time",
                &VTIME_DECISECONDS.to_string(),
            ])
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "stty failed for {path}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    /// Find a process holding `path` open, by walking `/proc/*/fd`.
    ///
    /// Read-only: it never signals anything it finds. Skips our own pid so a
    /// re-open within one run is not mistaken for contention.
    ///
    /// **Compares canonical paths.** Boards are addressed by their stable
    /// `/dev/serial/by-id/...` symlink, but `/proc/<pid>/fd/<n>` always
    /// resolves to the real device node — so a literal string comparison
    /// would find no holder for any board, ever, and this refusal would
    /// silently stop refusing. The failure it prevents (two readers
    /// splitting one byte stream, both seeing holes) is silent too, which
    /// is what makes a silently-disabled check the wrong kind of bug to
    /// have.
    fn holder(path: &str) -> Option<(u32, String)> {
        let me = std::process::id();
        let want = fs::canonicalize(path).unwrap_or_else(|_| path.into());
        for entry in fs::read_dir("/proc").ok()?.flatten() {
            let pid: u32 = match entry.file_name().to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if pid == me {
                continue;
            }
            let Ok(fds) = fs::read_dir(entry.path().join("fd")) else { continue };
            for fd in fds.flatten() {
                if fs::read_link(fd.path()).is_ok_and(|t| t == want) {
                    let cmd = fs::read_to_string(format!("/proc/{pid}/cmdline")).map_or_else(
                        |_| "unknown".to_string(),
                        |c| c.split('\0').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "),
                    );
                    return Some((pid, cmd));
                }
            }
        }
        None
    }

    /// The device this is attached to — for error messages that name the port
    /// rather than leaving the reader to guess which board failed.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Send bytes to the board (the session trigger, a console command).
    ///
    /// # Errors
    /// If the write fails — a vanished port, typically.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.port.write_all(bytes)?;
        self.port.flush()
    }

    /// Is the device at `path` back and usable?
    ///
    /// **Openable, not merely present**: after a reset the node reappears in
    /// `devtmpfs` a moment before the endpoint will accept an `open`, so an
    /// existence check passes and the `open` after it fails.
    ///
    /// ## Read-only about the PORT, not about the STREAM
    ///
    /// **Never poll this in front of a reader.** On Linux `cdc_acm` an open
    /// submits read URBs and the close discards whatever they fetched, so a
    /// poll loop consumes the very bytes it causes to arrive — including the
    /// identity, which lands at ~0.4 s of uptime. Where something wants those
    /// bytes, open for real and let a failed open be the signal (see
    /// [`crate::board::reset_and_reopen`]). This is the right probe only where
    /// nothing wants the stream: a character-device check, or waiting for a
    /// board to become addressable by `espflash`.
    ///
    /// # Errors
    /// A string naming what was wrong — absent, not a character device, or not
    /// yet openable — since that is what a bounded wait reports on giving up.
    pub fn openable(path: &str) -> Result<(), String> {
        let meta = fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if !meta.file_type().is_char_device() {
            return Err(format!("{path} exists but is not a character device"));
        }
        OpenOptions::new().read(true).open(path).map(|_| ()).map_err(|e| format!("{path} not yet openable: {e}"))
    }
}

impl Source for SerialSource {
    fn read_available(&mut self, budget: Duration) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + budget;
        let mut buf = [0u8; 4096];
        loop {
            match self.port.read(&mut buf) {
                // A kernel read timeout with `VMIN=0` returns 0 bytes rather
                // than an error. Distinguishing that from a real EOF is not
                // possible here and does not need to be: the caller's deadline
                // is what ends the wait either way.
                Ok(0) => {
                    if Instant::now() >= deadline {
                        return Ok(Vec::new());
                    }
                }
                Ok(n) => return Ok(buf[..n].to_vec()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opening a device that does not exist must fail with a message naming it,
    /// not panic and not hang. A mistyped MAC or an unplugged board is the
    /// commonest way to start a run wrongly.
    #[test]
    fn opening_a_missing_port_fails_with_the_path_in_the_message() {
        let e =
            SerialSource::open("/dev/definitely-not-a-tty-xyz", 115_200).expect_err("a missing device must not open");
        assert!(e.to_string().contains("definitely-not-a-tty-xyz"), "the error must name the port: {e}");
    }

    #[test]
    fn openable_names_an_absent_device() {
        let e = SerialSource::openable("/dev/definitely-not-a-tty-xyz").expect_err("absent must not be openable");
        assert!(e.contains("definitely-not-a-tty-xyz"), "must name the port: {e}");
    }

    /// The `is_char_device` half, which is the half that matters: a stale
    /// regular file left at a device path would pass an existence check and
    /// then deliver nothing forever.
    #[test]
    fn openable_rejects_something_that_exists_but_is_not_a_device() {
        let p = std::env::temp_dir().join(format!("hil-openable-{}", std::process::id()));
        fs::write(&p, b"not a tty").expect("write temp file");
        let e = SerialSource::openable(&p.to_string_lossy()).expect_err("a regular file is not a port");
        let _ = fs::remove_file(&p);
        assert!(e.contains("not a character device"), "must say why: {e}");
    }
}
/// The on-target tier: what only a live board can prove.
///
/// # Prerequisite (`conventions/testing.md` §4)
///
/// Every test here is `#[ignore]`d because it needs **the rig**:
///
/// - a board running firmware built with `m5stack-core`'s **`serial-cmd`**
///   feature — the precondition is a firmware that can *receive* a byte, and
///   that feature is what decides it;
/// - `M5STACK_HIL_BOARD` set to its **MAC** — never a `ttyACM` number, which
///   renumbers on replug (§1).
///
/// The BSP owns no command vocabulary, so the probe byte and the reply it
/// should produce are the caller's. Choose a verb that is observable and
/// **inert**; one that starts work or resets the board is not a probe.
///
/// ```sh
/// M5STACK_HIL_BOARD=1C:DB:D4:BA:83:38 \
///   M5STACK_HIL_PROBE=v M5STACK_HIL_PROBE_REPLY='identity:' \
///   cargo test -- --ignored
/// ```
///
/// An unset variable **fails** rather than passing vacuously: a hardware test
/// that silently skips is the "green because unrun" defect this crate exists to
/// remove, and nothing else covers this tier.
///
/// Host fakes prove every decision *about* bytes; what they cannot reach is
/// whether `stty` took effect, whether `VMIN`/`VTIME` really returns on the
/// first byte, whether `Drop` really closes an fd, whether a write reaches a
/// board, and whether the `/proc` scan sees a foreign process.
///
/// A physical port is shared mutable state and two of these assert that a
/// second opener is refused, so they take [`RIG`] and serialise themselves
/// rather than relying on `--test-threads=1` being remembered.
#[cfg(test)]
mod ontarget {
    use std::{
        process::{Child, Stdio},
        sync::{Mutex, MutexGuard},
    };

    use super::{Duration, Instant, SerialSource, fs};
    use crate::{
        listen::{Listener, Outcome, Source},
        wait,
    };

    /// One board, one test at a time. Poisoning is ignored deliberately: a
    /// panicking test has already reported, and turning that into a cascade of
    /// unrelated failures hides which one actually broke.
    static RIG: Mutex<()> = Mutex::new(());

    fn rig() -> MutexGuard<'static, ()> {
        RIG.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    const BAUD: u32 = 1_000_000;

    /// A required environment variable, or a failure that says how to run this
    /// tier.
    ///
    /// A panic rather than a silent skip, deliberately: a hardware test that
    /// quietly passes when its prerequisite is absent is the "green because
    /// unrun" defect this crate exists to remove, and it is worse here than
    /// elsewhere because nothing else covers this tier.
    fn required(var: &str, what: &str) -> String {
        std::env::var(var).unwrap_or_else(|_| {
            panic!(
                "{var} is unset ({what}). This tier needs the rig:\n  \
                 M5STACK_HIL_BOARD=<board-MAC> M5STACK_HIL_PROBE=<byte> \
                 M5STACK_HIL_PROBE_REPLY=<substring> cargo test -- --ignored\n\
                 The board must run firmware built with m5stack-core's `serial-cmd` \
                 feature, so it can receive a command byte at all."
            )
        })
    }

    /// The board's stable path, built the one way the crate builds it — via
    /// [`crate::board::Board::cores3`], so the tests and the tool cannot
    /// disagree about how a board is addressed.
    fn tty() -> String {
        crate::board::Board::cores3(&required("M5STACK_HIL_BOARD", "the board's MAC")).port
    }

    /// Is `name` present in `stty -a` output, and is it ON?
    ///
    /// `Some(true)` enabled, `Some(false)` disabled (`-name`), `None` absent.
    ///
    /// A plain `contains()` is NOT a test for this and it cost a false pass:
    /// `contains("clocal")` also matches `-clocal`, and `contains("-echo")`
    /// matches `-echoe`, so both assertions here passed with the setting they
    /// checked switched OFF. Found by mutation — deleting `clocal` from the
    /// configuration left the test green. Flags are whitespace/`;`-separated
    /// tokens, so compare tokens.
    fn flag(cfg: &str, name: &str) -> Option<bool> {
        cfg.split([' ', ';', '\t']).filter(|t| !t.is_empty()).find_map(|t| match t.strip_prefix('-') {
            Some(rest) if rest == name => Some(false),
            _ if t == name => Some(true),
            _ => None,
        })
    }

    /// How many of OUR fds point at `path`. The direct assertion that `Drop`
    /// closed the descriptor: [`SerialSource::holder`] cannot answer it,
    /// because it skips our own pid on purpose.
    fn own_fds_on(path: &str) -> usize {
        // Canonicalise: we address the board by its `/dev/serial/by-id/...`
        // symlink, but `/proc/self/fd` always resolves to the real device node,
        // so a literal comparison would count zero fds and this test would pass
        // for the wrong reason — it would stop testing `Drop` entirely.
        let want = fs::canonicalize(path).unwrap_or_else(|_| path.into());
        fs::read_dir("/proc/self/fd")
            .map_or(0, |fds| fds.flatten().filter(|fd| fs::read_link(fd.path()).is_ok_and(|t| t == want)).count())
    }

    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn a_live_port_opens_configured_and_reports_its_path() {
        let _rig = rig();
        let t = tty();
        let s = SerialSource::open(&t, BAUD).expect("a live board must open");
        assert_eq!(s.path(), t, "the source must name the port it holds");
    }

    /// `stty` is shelled out to rather than set from Rust, so nothing on the
    /// host proves it took effect. Ask the kernel back.
    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn stty_really_configured_raw_mode_and_the_read_timeout() {
        let _rig = rig();
        let t = tty();
        let _s = SerialSource::open(&t, BAUD).expect("open");
        let out = std::process::Command::new("stty").args(["-F", &t, "-a"]).output().expect("stty -a");
        let cfg = String::from_utf8_lossy(&out.stdout).replace('\n', " ");
        assert!(cfg.contains("speed 1000000 baud"), "baud must be set: {cfg}");
        assert!(cfg.contains("min = 0"), "VMIN must be 0 or a silent read blocks forever: {cfg}");
        assert!(cfg.contains("time = 1"), "VTIME must be 1 decisecond: {cfg}");
        assert!(flag(&cfg, "echo").is_some_and(|on| !on), "echo feeds our own triggers back as board output: {cfg}");
        // The three that make a capture safe rather than merely correct.
        //
        // MUTATING THESE: delete one from `configure` and this test still passes,
        // which is NOT a pass — termios is sticky on the device, so a value an
        // earlier run set survives until the port re-enumerates. To test one,
        // actively set its opposite (`-echo` -> `echo`), which is how the `echo`
        // assertion above was proven and how the `contains()` bug below was found.
        //
        // Against a pristine device only `-hupcl` is a real change here: the first
        // capture taken on this rig showed `cs8 hupcl … cread clocal -crtscts`, so
        // `clocal` and `-crtscts` were already the defaults and setting them is
        // defensive rather than load-bearing on THIS adapter. They stay because
        // the next one need not agree, and because a capture that quietly depends
        // on a driver default is the kind of thing that breaks on a new host.
        assert!(flag(&cfg, "clocal").is_some_and(|on| on), "without clocal a read can hang up on carrier: {cfg}");
        assert!(
            flag(&cfg, "hupcl").is_some_and(|on| !on),
            "hupcl lowers DTR on close, which is how esptool RESETS this chip: {cfg}"
        );
        assert!(
            flag(&cfg, "crtscts").is_some_and(|on| !on),
            "the CDC device implements no hardware flow control: {cfg}"
        );
    }

    /// The ordering `open` exists to guarantee: the discipline must be in force
    /// while WE hold the descriptor, not merely have been set at some point by
    /// a process that then closed it. Re-read it through a second,
    /// independent `stty` while our source is alive — if the settings had
    /// lapsed with the configuring descriptor, this is where it shows.
    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn the_discipline_holds_while_we_own_the_port() {
        let _rig = rig();
        let t = tty();
        let s = SerialSource::open(&t, BAUD).expect("open");
        // Something else touching the port must not silently undo us either: put
        // it back to cooked with echo, as a stray `stty` or a flashing tool would,
        // and confirm the source notices rather than capturing corrupted bytes.
        let out = std::process::Command::new("stty").args(["-F", &t, "-a"]).output().expect("stty -a");
        let before = String::from_utf8_lossy(&out.stdout).replace('\n', " ");
        assert!(before.contains("-echo") && before.contains("min = 0"), "our settings must be live: {before}");
        drop(s);
    }

    /// The claim in this module's docs: with `VMIN=0`/`VTIME` the wait happens
    /// **in the kernel**, so a silent board costs a bounded block and not a
    /// spin. A non-blocking fd would return instantly and burn the budget
    /// in userspace.
    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn a_silent_read_blocks_in_the_kernel_and_honours_the_budget() {
        let _rig = rig();
        let mut s = SerialSource::open(&tty(), BAUD).expect("open");
        // Drain whatever the board happened to be saying, so the timed read
        // below is measuring silence and not backlog.
        let _ = s.read_available(Duration::from_millis(300));

        let budget = Duration::from_millis(500);
        let t0 = Instant::now();
        let got = s.read_available(budget).expect("a silent port is not an error");
        let took = t0.elapsed();

        assert!(took < budget + Duration::from_millis(400), "must honour the budget, took {took:?}");
        if got.is_empty() {
            assert!(
                took >= budget.mul_f32(0.8),
                "an empty return must mean it BLOCKED for the budget, not spun: {took:?}"
            );
        }
    }

    /// The entire argument for this crate being in Rust. Asserted on our own
    /// `/proc/self/fd`, which is the only thing that can see it.
    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn drop_really_closes_the_descriptor() {
        let _rig = rig();
        let t = tty();
        let before = own_fds_on(&t);
        {
            let _s = SerialSource::open(&t, BAUD).expect("open");
            assert_eq!(own_fds_on(&t), before + 1, "holding the port must show one fd");
        }
        assert_eq!(own_fds_on(&t), before, "Drop must close it — this is why the harness is Rust");
    }

    /// The `/proc` holder scan, which the host suite structurally cannot test:
    /// it skips our own pid, so contention needs a second *process*. This is
    /// the §8.1 half that refuses rather than reclaims — and the failure it
    /// prevents is a split byte stream, where both readers see holes.
    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn a_foreign_holder_is_refused_and_named_never_killed() {
        let _rig = rig();
        let t = tty();
        // A holder that OPENS the port and then sits, rather than one that reads
        // it. `cat <tty>` is the obvious choice and it is wrong: this module
        // configures `VMIN=0`/`VTIME=1`, so a read on a silent board returns
        // zero bytes, `cat` takes that for EOF and exits within ~100 ms. The
        // test then passes or fails on whether the check won a race — measured
        // 2026-07-30, the squatter was already gone one second after spawning.
        // Holding an fd with no read is deterministic and is what the real
        // offender (an orphaned reader) does anyway.
        let mut squatter: Child = std::process::Command::new("sh")
            // `exec sleep`, not `sleep`: it replaces the shell in place, so
            // exactly ONE process holds the fd and its pid is the one we spawned.
            // A forking shell would leave two holders and the scan could name
            // either, making the assertion below a coin toss.
            .args(["-c", &format!("exec 3< '{t}'; exec sleep 30")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a squatter");
        let pid = squatter.id();

        // Wait for it to actually hold the port — spawn returns before open().
        let held =
            wait::until("the squatter to hold the port", Duration::from_secs(5), Duration::from_millis(20), || {
                if SerialSource::open(&t, BAUD).is_err() { Ok(()) } else { Err("not holding it yet".into()) }
            });

        let e = SerialSource::open(&t, BAUD).err();
        let alive_after = fs::metadata(format!("/proc/{pid}")).is_ok();
        let _ = squatter.kill();
        let _ = squatter.wait();

        held.expect("the squatter never took the port");
        let e = e.expect("a foreign holder must be refused");
        assert_eq!(e.kind(), std::io::ErrorKind::AddrInUse, "must be AddrInUse, got {e:?}");
        let m = e.to_string();
        assert!(m.contains(&pid.to_string()), "must name the holder's pid: {m}");
        assert!(m.contains("NOT killing it"), "must say it is not reclaiming: {m}");
        assert!(alive_after, "§8.1: the holder must still be alive — this must refuse, never reclaim");
    }

    /// A write must reach the board and the board must answer.
    ///
    /// The probe is the caller's, because the BSP owns no command vocabulary —
    /// `serial-cmd` hands the firmware a `Read` endpoint and the parser is
    /// downstream. The module docs say what makes a verb suitable: observable,
    /// and inert. This test cannot check inertness, so it does not pretend to;
    /// choosing a verb that starts work is a way to break your own board, not
    /// something an assertion here can catch.
    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn a_trigger_reaches_the_board_and_it_answers() {
        let _rig = rig();
        let probe = required("M5STACK_HIL_PROBE", "the console byte to send");
        let expect = required("M5STACK_HIL_PROBE_REPLY", "the substring the board should answer with");

        let port = SerialSource::open(&tty(), BAUD).expect("open");
        let mut l = Listener::new(port);

        l.source_mut().expect("attached").send(probe.as_bytes()).expect("send the probe");
        match l.wait_for_line(&expect, Duration::from_secs(5)) {
            Outcome::Matched(line) => assert!(line.contains(&expect), "{line}"),
            other => panic!(
                "the board did not answer {probe:?} with {expect:?}: {other:?}\ncaptured: {:?}",
                String::from_utf8_lossy(l.bytes())
            ),
        }
    }

    /// The probe a bounded reset-wait is pointed at. On a board that is present
    /// it must succeed immediately — if this were slow or flaky, every
    /// re-enumeration wait built on it would inherit that.
    #[test]
    #[ignore = "needs the rig: a CoreS3 built with m5stack-core/serial-cmd, M5STACK_HIL_BOARD=<MAC>"]
    fn openable_succeeds_at_once_on_a_present_board() {
        let _rig = rig();
        let t = tty();
        let t0 = Instant::now();
        SerialSource::openable(&t).expect("a present board must be openable");
        assert!(t0.elapsed() < Duration::from_millis(500), "the probe must be cheap: {:?}", t0.elapsed());
    }
}

/// A [`Source`] that drains the port **continuously**, on its own thread.
///
/// [`SerialSource`] reads only when someone calls it, and the harness only
/// calls it while waiting for a pattern — so between a match and the next wait
/// nobody is reading, and the board's output accumulates in the kernel's tty
/// buffer. That buffer overflows silently: no error to the reader, no signal to
/// the board.
///
/// Measured 2026-07-31: two chunks (64 B and ~128 B) vanished mid-line across
/// ~718 sessions, with the firmware's own ring reporting **zero** dropped bytes
/// — and that ring's accounting is known-good, having been shown to mark every
/// byte it dropped under two deliberate starvation tests. Loss the firmware
/// never saw, on a write path whose error type is `Infallible`, is loss that
/// happened after the bytes left the chip.
///
/// So the port is drained by a thread that never stops, and waiting consumes
/// from what it has already collected. The fix is structural: there is no
/// window in which nobody is reading, because reading no longer depends on
/// anyone asking.
pub struct DrainedSource {
    /// Bytes collected so far, oldest first.
    buf: Arc<Mutex<Vec<u8>>>,
    /// Set on drop so the reader thread exits rather than leaking per reset.
    stop: Arc<AtomicBool>,
    /// First read error, kept so the next `read_available` can report it
    /// instead of the thread dying silently.
    err: Arc<Mutex<Option<String>>>,
    /// Write side. Separate descriptor: see `open`.
    tx: File,
    path: String,
}

impl DrainedSource {
    /// Open `path` and start draining it immediately.
    ///
    /// # Errors
    /// If the port cannot be opened or configured.
    pub fn open(path: &str, baud: u32) -> io::Result<Self> {
        let src = SerialSource::open(path, baud)?;
        // A second descriptor for writing. The reader thread owns the first and
        // blocks in `read`, so the trigger write cannot share it — and must not
        // wait behind it either.
        let tx = src.port.try_clone()?;
        let mut port = src.port;
        let buf = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let err = Arc::new(Mutex::new(None));
        let (b, s, e) = (Arc::clone(&buf), Arc::clone(&stop), Arc::clone(&err));
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while !s.load(Ordering::Relaxed) {
                match port.read(&mut chunk) {
                    // `VMIN=0`/`VTIME` makes a quiet port return 0, not an
                    // error. Keep going: silence is not the end of the stream.
                    Ok(0) => {}
                    Ok(n) => b.lock().map_or((), |mut g| g.extend_from_slice(&chunk[..n])),
                    Err(ref x) if x.kind() == io::ErrorKind::Interrupted => {}
                    Err(x) => {
                        *e.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(x.to_string());
                        return;
                    }
                }
            }
        });
        Ok(Self { buf, stop, err, tx, path: src.path })
    }

    /// The device path, for errors that must name the board.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Send bytes to the board (the session trigger, a console command).
    ///
    /// # Errors
    /// If the write fails — a vanished port, typically.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.tx.write_all(bytes)?;
        self.tx.flush()
    }
}

impl Drop for DrainedSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Source for DrainedSource {
    fn read_available(&mut self, budget: Duration) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(e) = self.err.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
                return Err(io::Error::other(e));
            }
            {
                let mut g = self.buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !g.is_empty() {
                    return Ok(core::mem::take(&mut *g));
                }
            }
            if Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            // The reader thread is what makes progress; this only waits for it.
            // Short enough that a deadline is honoured promptly, long enough
            // not to spin a core while the board is quiet.
            std::thread::sleep(Duration::from_millis(5).min(budget));
        }
    }
}
