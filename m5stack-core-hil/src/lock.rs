// SPDX-License-Identifier: MIT OR Apache-2.0
//! One owner per board: refused loudly, released always.
//!
//! Boundary: **an exclusive claim on a named resource, and its release.**
//! Reading bytes fails that sentence ([`crate::serial`]).
//!
//! `conventions/testing.md` §8.1 asks for three things: acquire exclusively,
//! **refuse and report rather than reclaim** when someone else holds it, and
//! release on every exit path. The middle one is the one tooling gets wrong —
//! killing the holder destroys another session's live capture.
//!
//! Keyed to the **MAC, never the `ttyN` index**: enumeration indices renumber
//! on replug, so a lock keyed to one lets two runs claim the same board under
//! two names.
//!
//! A lockfile whose PID is gone is stale and may be taken over, and that path
//! is load-bearing rather than defensive: **`Drop` does not run on a signal**,
//! so a `SIGTERM`ed run leaves its lockfile behind (verified on the rig). `std`
//! has no signal handling; the next run takes the stale lock and **says so**,
//! which keeps the rig unwedged and the death visible.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process,
};

/// An exclusive claim on one board, released on drop.
///
/// Holds no file handle: the lockfile's *existence* plus a live PID inside it
/// is the lock. That way a holder is identifiable by anyone — including a human
/// with `cat` — rather than being an invisible kernel-side flag.
#[derive(Debug)]
pub struct BoardLock {
    path: PathBuf,
    /// False when the lock was taken over from a stale holder, so the message
    /// can say so rather than pretending the rig was clean.
    took_over_stale: bool,
}

impl BoardLock {
    /// Claim `board_id` (a MAC — never a tty index) under `dir`.
    ///
    /// # Panics
    /// If `board_id` looks like a tty path or index rather than a stable
    /// identity. That is a programming error, not a runtime condition: a lock
    /// keyed to `ttyACM0` would let two runs claim one board under two names,
    /// which is the failure the lock exists to prevent.
    ///
    /// # Errors
    /// [`io::ErrorKind::AddrInUse`] if a **live** holder has it, with a message
    /// naming the PID, its command line, and how to free it. Any other error is
    /// a filesystem problem creating the lock.
    pub fn acquire(dir: &Path, board_id: &str) -> io::Result<Self> {
        assert!(
            !board_id.starts_with("/dev/") && !board_id.starts_with("tty"),
            "lock must be keyed to a stable board identity (MAC), not a tty index: {board_id}"
        );
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.lock", board_id.replace(':', "-")));

        let mut took_over_stale = false;
        if let Some(pid) = Self::holder(&path) {
            if Self::alive(pid) {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "board {board_id} is already held by pid {pid} ({}).\n\
                         NOT killing it — it may be another run, a live capture, or your own work.\n\
                         To free it deliberately:  kill {pid}   (then rm {})",
                        Self::cmdline(pid).unwrap_or_else(|| "command line unavailable".into()),
                        path.display()
                    ),
                ));
            }
            took_over_stale = true;
        }

        let mut f = fs::File::create(&path)?;
        write!(f, "{}", process::id())?;
        f.flush()?;
        Ok(Self { path, took_over_stale })
    }

    /// Whether this claim displaced a dead holder's lockfile. Worth reporting:
    /// it means a previous run died without releasing, which is itself news.
    #[must_use]
    pub fn took_over_stale(&self) -> bool {
        self.took_over_stale
    }

    /// The pid recorded in a lockfile, if it is readable and numeric.
    fn holder(path: &Path) -> Option<u32> {
        fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// Is `pid` still running? `/proc/<pid>` rather than `kill -0`, because
    /// this must not send a signal to a process it has been told not to
    /// disturb.
    fn alive(pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    /// The holder's command line, so the report names what to look at rather
    /// than leaving a bare number.
    fn cmdline(pid: u32) -> Option<String> {
        let raw = fs::read_to_string(format!("/proc/{pid}/cmdline")).ok()?;
        let joined = raw.split('\0').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
        if joined.is_empty() { None } else { Some(joined) }
    }
}

impl Drop for BoardLock {
    /// Release on every path — normal return, error, and panic. This is the
    /// half the bash harness only has by way of a `trap` added today, and a
    /// `trap` is a thing to remember whereas this is a thing that happens.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hil-lock-test-{}-{}", process::id(), name));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_claim_creates_a_lockfile_holding_our_pid() {
        let d = tmp("basic");
        let l = BoardLock::acquire(&d, "1C:DB:D4:BA:83:38").expect("fresh rig");
        let recorded = fs::read_to_string(&l.path).expect("lockfile readable");
        assert_eq!(recorded.trim(), process::id().to_string());
        assert!(!l.took_over_stale());
    }

    /// Release must happen on drop, not at the end of a happy path.
    #[test]
    fn dropping_the_lock_frees_the_board() {
        let d = tmp("drop");
        let path = {
            let l = BoardLock::acquire(&d, "AA:BB:CC:DD:EE:FF").expect("fresh");
            l.path.clone()
        };
        assert!(!path.exists(), "the lockfile must be gone once the lock is dropped");
    }

    /// And on panic — the path a `trap`-less script misses entirely.
    #[test]
    fn a_panic_still_frees_the_board() {
        let d = tmp("panic");
        let id = "11:22:33:44:55:66";
        let expected = d.join("11-22-33-44-55-66.lock");
        let r = std::panic::catch_unwind(|| {
            let _l = BoardLock::acquire(&d, id).expect("fresh");
            panic!("simulated mid-run failure");
        });
        assert!(r.is_err(), "the panic must propagate");
        assert!(!expected.exists(), "Drop must still have released the board");
    }

    /// A LIVE holder is refused and reported — never reclaimed. This is the
    /// rule the bash harness breaks by walking /proc and killing whoever it
    /// finds, which destroyed a live capture earlier today.
    #[test]
    fn a_live_holder_is_refused_with_pid_and_command_line() {
        let d = tmp("live");
        let id = "DE:AD:BE:EF:00:01";
        let _held = BoardLock::acquire(&d, id).expect("first claim");
        let e = BoardLock::acquire(&d, id).expect_err("second claim must be refused");
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
        let m = e.to_string();
        assert!(m.contains(&process::id().to_string()), "must name the holder's pid: {m}");
        assert!(m.contains("NOT killing it"), "must say it is not reclaiming: {m}");
        assert!(m.contains("kill "), "must say how to free it deliberately: {m}");
    }

    /// A crashed run must not wedge the rig forever — but staleness is CHECKED,
    /// and the takeover is reported rather than silent.
    #[test]
    fn a_stale_lockfile_is_taken_over_and_the_takeover_is_reported() {
        let d = tmp("stale");
        let id = "DE:AD:BE:EF:00:02";
        fs::create_dir_all(&d).expect("mkdir");
        // A pid that cannot be running: the kernel's maximum is far below this.
        fs::write(d.join("DE-AD-BE-EF-00-02.lock"), "4294967295").expect("write stale lock");
        let l = BoardLock::acquire(&d, id).expect("a dead holder must not wedge the rig");
        assert!(l.took_over_stale(), "the takeover must be reported, not hidden");
    }

    /// Keying the lock to a tty index would let two runs claim one board under
    /// two names — the exact failure the lock exists to prevent, since ttyACM
    /// numbers renumber on replug.
    #[test]
    #[should_panic(expected = "stable board identity")]
    fn keying_the_lock_to_a_tty_is_refused() {
        let d = tmp("tty");
        let _ = BoardLock::acquire(&d, "/dev/ttyACM0");
    }
}
