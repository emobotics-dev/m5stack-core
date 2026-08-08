// SPDX-License-Identifier: MIT OR Apache-2.0
//! Never stop listening: one raw byte buffer, drained for the whole run.
//!
//! Boundary: **bytes accumulated for the run, and the decision to stop waiting
//! for a pattern.** Opening a port fails that sentence; so does deciding what
//! the bytes mean ([`crate::report`]).
//!
//! A runner that attaches and detaches around each attempt loses output three
//! ways — a gap while nothing is attached, truncation of the failing attempt's
//! evidence by the recovery from it (`conventions/testing.md` §8.2), and a poll
//! loop over a file (§1). Owning the source for the run removes all three.
//!
//! **Bytes, not lines.** An earlier revision split on `\n` as bytes arrived and
//! held the unterminated tail aside until its newline came. A board that dies
//! mid-sentence never sends it, so the panic or half-written dump — the one
//! moment the tail *is* the story — never reached the evidence. Lines are a
//! derived view. Searching raw bytes also matches inside a line that has not
//! ended yet. A whole run is tens of kilobytes; being clever here loses
//! evidence and saves nothing.
//!
//! **A timed read, not a thread**, because the kernel already offers one:
//! termios `VMIN = 0`, `VTIME = t` returns the moment a byte arrives, or after
//! `t` if none does. `t` bounds only the *silent* case — nothing wakes on a
//! timer while the board is talking.

use std::{
    io,
    time::{Duration, Instant},
};

/// Somewhere bytes come from, with a bounded wait.
pub trait Source {
    /// Return bytes that have arrived, waiting up to `budget` for the first.
    ///
    /// An empty return means `budget` elapsed with nothing — a silence, not an
    /// error. Callers must keep `Ok(vec![])` and `Err` apart: the first means
    /// "quiet so far", the second means the port is gone.
    ///
    /// # Errors
    /// Whatever the underlying source raises.
    fn read_available(&mut self, budget: Duration) -> io::Result<Vec<u8>>;
}

/// Why a wait ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The pattern appeared. Carries the surrounding line for reporting — a
    /// *view* of the bytes, which remain in the buffer either way.
    Matched(String),
    /// The budget elapsed. §8.5: expiry is a FAIL that reports.
    DeadlineExpired,
    /// The source failed — a dead board or a lost port. Deliberately distinct
    /// from a deadline: waiting longer cannot help, and calling it a timeout
    /// sends the next person at the budget instead of at the port.
    SourceFailed(String),
}

/// The run's ears and its memory.
///
/// Keeps every byte it ever produced. Nothing shortens the buffer — that is how
/// §8.2 is enforced rather than remembered.
///
/// The **memory spans the run; the source spans a boot.** Those are different
/// lifetimes only because the hardware makes them so, and
/// [`Listener::across_reset`] documents why. A source is never detached
/// *within* a session — that is the attach/detach this module exists to
/// eliminate.
pub struct Listener<S: Source> {
    /// `None` for exactly one span: while the board is resetting and its port
    /// does not exist. [`Listener::across_reset`] is the only thing that can
    /// produce that state, and it is the only reason this is an `Option` — a
    /// reset must *release* the fd (a reset tool cannot open a port we hold),
    /// and the device is genuinely absent until it re-enumerates.
    ///
    /// A wait with no source reports [`Outcome::SourceFailed`] rather than
    /// looking like a silence, because "there is no port" and "the board is
    /// quiet" must never be confused.
    source: Option<S>,
    /// Where bytes are streamed as they arrive, if anywhere.
    ///
    /// `conventions/testing.md` §8.4: "Stream into it as the run executes,
    /// don't capture after… a capture deferred to a step that never runs is
    /// lost." Writing the buffer once at the end loses the whole run to a
    /// crash, a SIGTERM, or an early return — and a run that died is exactly
    /// when the transcript is worth having.
    sink: Option<Box<dyn io::Write>>,
    /// Every byte ever received, raw and uninterpreted.
    buffer: Vec<u8>,
    /// How far into `buffer` waits have already looked.
    ///
    /// A run-long buffer makes a false *pass* possible: session N's report is
    /// still present when session N+1 waits, and would satisfy it. Truncation
    /// used to hide that by destroying the history; keeping the history means
    /// staleness must be handled explicitly.
    cursor: usize,
    /// Where the CURRENT capture starts, for judgements about it — distinct
    /// from `cursor`, which waits advance as they scan.
    ///
    /// A console hole is such a judgement. Keying it off `cursor` was wrong:
    /// merely *reading* a line moved the cursor past the marker, so a hole
    /// stopped being visible the moment anything scanned past it. Keying it
    /// off the whole buffer was also wrong: a hole then became permanent, so
    /// re-reading could never clear one and a retry was inert.
    capture_floor: usize,
}

impl<S: Source> Listener<S> {
    /// Start listening. The source is owned from here until the run ends.
    pub fn new(source: S) -> Self {
        Self { source: Some(source), buffer: Vec::new(), cursor: 0, capture_floor: 0, sink: None }
    }

    /// Stream every arriving byte to `sink` as well as buffering it.
    ///
    /// Flushed on every read, deliberately: an unflushed buffer is the same
    /// deferred capture §8.4 forbids, just moved into libc.
    #[must_use]
    pub fn streaming_to(mut self, sink: Box<dyn io::Write>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Every byte received so far, raw. Append-only.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.buffer
    }

    /// Swap in a new source, keeping every byte, the cursor and the sink.
    ///
    /// A hardware fact, not a loosening of the invariant: on the ESP32-S3 the
    /// USB-Serial-JTAG resets *with the chip*, so the port disappears and the
    /// fd we held becomes invalid. No owner survives that — the question is
    /// only whether the *evidence* does. So the buffer, cursor and sink belong
    /// to the **run** and only the fd belongs to the boot. Keeping the cursor
    /// stops a pre-reset session's report satisfying a post-reset wait.
    ///
    /// **The order is the contract.** The old source is dropped *before*
    /// `reconnect` runs, because a reset tool cannot open a port this process
    /// holds. `reconnect` therefore owns the whole span in which the device
    /// does not exist.
    ///
    /// # Errors
    /// Whatever `reconnect` reports. On failure the listener is left with **no
    /// source**, so later waits return [`Outcome::SourceFailed`] rather than a
    /// silence that reads like a quiet board.
    pub fn across_reset<E>(&mut self, reconnect: impl FnOnce() -> Result<S, E>) -> Result<(), E> {
        // Drop first, reconnect second. Assigning `Some(reconnect()?)` in one
        // statement would hold the old fd open across the reset and the reset
        // would fail — the bug this signature exists to make unwritable.
        self.source = None;
        self.source = Some(reconnect()?);
        Ok(())
    }

    /// The source, for the caller to *write* to between waits.
    ///
    /// The listener owns the source for the whole run — that is the invariant
    /// this module exists to hold — so a caller that needs to send a trigger
    /// cannot keep its own handle alongside. Rather than hand the port back and
    /// forth (which is the attach/detach this replaces), it is borrowed here.
    ///
    /// `None` while the board is resetting ([`Listener::across_reset`]), or
    /// after a reconnect failed. Callers must treat that as the hard error it
    /// is rather than skipping the send.
    ///
    /// Deliberately not a `send` wrapper: writing is not this module's concern,
    /// and forwarding it would drag a second responsibility across the boundary
    /// stated at the top of the file.
    pub fn source_mut(&mut self) -> Option<&mut S> {
        self.source.as_mut()
    }

    /// The buffer rendered as lines — a derived view for reporting, never the
    /// storage. A trailing fragment with no newline is included, because that
    /// fragment is often a dying board's last words.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.buffer)
            .split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Block until `pattern` appears in bytes no earlier wait has examined, or
    /// `budget` elapses.
    ///
    /// Matches the **instant the pattern appears**, even mid-line. Use this for
    /// anything whose arrival is the news — a panic, a `Guru Meditation`, a
    /// reset banner — where waiting for a newline that may never come is the
    /// bug. The returned line is whatever context exists so far and may be
    /// partial; if you intend to *parse* it, use [`Listener::wait_for_line`].
    pub fn wait_for(&mut self, pattern: &str, budget: Duration) -> Outcome {
        self.wait(pattern, budget, false)
    }

    /// Block until `pattern` appears **and its line is newline-terminated**.
    ///
    /// Use this when the line will be parsed. A serial read lands mid-line as a
    /// matter of course, so a `wait_for` that returned on first sight would
    /// hand back `"demo report: handsh"` and the parse would fail on output
    /// that was merely incomplete, not wrong.
    ///
    /// The cost is honest and bounded: if the board dies before the newline,
    /// this reports [`Outcome::DeadlineExpired`] — and the bytes are still in
    /// the buffer, so the truncated line remains evidence.
    pub fn wait_for_line(&mut self, pattern: &str, budget: Duration) -> Outcome {
        self.wait(pattern, budget, true)
    }

    fn wait(&mut self, pattern: &str, budget: Duration, need_newline: bool) -> Outcome {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(hit) = self.scan(pattern, need_newline) {
                return Outcome::Matched(hit);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Outcome::DeadlineExpired;
            }
            // No port is not a silence. A detached listener can never satisfy a
            // wait, so blocking out the deadline would spend the budget to reach
            // a conclusion already known — and report it as a timeout, sending
            // the next reader at the budget instead of at the reset that failed.
            let Some(source) = self.source.as_mut() else {
                return Outcome::SourceFailed(
                    "no source attached — a reset released the port and never got it back".into(),
                );
            };
            match source.read_available(remaining) {
                Ok(bytes) => self.absorb(&bytes),
                Err(e) => return Outcome::SourceFailed(e.to_string()),
            }
        }
    }

    /// Look for `pattern` at or after the cursor; on a hit, advance the cursor
    /// past its line and return that line.
    ///
    /// With `need_newline`, a hit whose line has not been terminated yet is
    /// **not** a hit — and the cursor does not move, so the same bytes are
    /// reconsidered once more arrive. Moving it would consume the partial line
    /// and the completed one could never be found.
    /// Keep `bytes` for the run, and stream them out if a sink is attached.
    fn absorb(&mut self, bytes: &[u8]) {
        if let Some(sink) = self.sink.as_mut() {
            // A failed write must not kill a run that is otherwise fine, but it
            // must not pass silently either.
            if let Err(e) = sink.write_all(bytes).and_then(|()| sink.flush()) {
                eprintln!("harness: transcript write failed, continuing in memory: {e}");
                self.sink = None;
            }
        }
        self.buffer.extend_from_slice(bytes);
    }

    /// Refuse to match anything captured so far. The bytes stay in the
    /// transcript; only waits stop seeing them.
    ///
    /// Drawn before a reset, this is what stops a wait matching the *previous*
    /// boot — see [`Listener::quiesce`], which is what makes the line sound.
    pub fn discard_backlog(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// The bytes still eligible to match — everything since the last
    /// [`begin_capture`](Self::begin_capture), or the whole capture if there
    /// has not been one.
    ///
    /// Distinct from [`bytes`](Self::bytes), which is the *transcript* and must
    /// stay complete. A judgement about "this capture" wants this one: a
    /// console hole belonging to a boot that was explicitly excluded should
    /// not condemn the boot after it — the same kind of line
    /// [`discard_backlog`](Self::discard_backlog) draws for waits, but a
    /// distinct field, because merely *reading* past a marker (which moves the
    /// cursor `discard_backlog` keys off) must not make a hole stop being
    /// visible.
    #[must_use]
    pub(crate) fn fresh_bytes(&self) -> &[u8] {
        &self.buffer[self.capture_floor.min(self.buffer.len())..]
    }

    /// Begin a new capture: judgements like [`board::console_hole`] stop
    /// looking at anything received so far.
    ///
    /// Deliberately separate from [`discard_backlog`](Self::discard_backlog),
    /// which governs what WAITS may match — the two questions are different
    /// even though [`board::reset_attached`] is now the one place that answers
    /// both, at the same instant, which is what earlier callers pairing them by
    /// hand at three different call sites got wrong (see
    /// `board::reset_attached` for what that cost). `pub(crate)`, not
    /// `pub`: there is currently no reason to call this anywhere but there.
    pub(crate) fn begin_capture(&mut self) {
        self.capture_floor = self.buffer.len();
    }

    /// Read until the source has said nothing for `quiet_for`, or give up after
    /// `budget`. `true` if silence was established.
    ///
    /// A barrier alone is not enough to separate two boots: bytes emitted
    /// before a reset can still be in flight and land *after* the barrier, so
    /// a stale line remains matchable. Waiting for real silence first removes
    /// that — nothing is buffered and nothing is on its way, so everything
    /// arriving next was caused by the reset.
    ///
    /// `false` means a board that never stopped talking, and therefore a
    /// capture whose boots cannot be told apart. That is reported rather than
    /// assumed away: for `--until`-style waits it is the difference between a
    /// pass and a **false** pass.
    pub fn quiesce(&mut self, quiet_for: Duration, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            let Some(source) = self.source.as_mut() else { return false };
            // `read_available` returns empty exactly when nothing arrived in
            // the window, which is the definition of quiet being looked for.
            match source.read_available(quiet_for) {
                Ok(bytes) if bytes.is_empty() => return true,
                Ok(bytes) => self.absorb(&bytes),
                Err(_) => return false,
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    fn scan(&mut self, pattern: &str, need_newline: bool) -> Option<String> {
        let hay = String::from_utf8_lossy(&self.buffer[self.cursor.min(self.buffer.len())..]).into_owned();
        let at = hay.find(pattern)?;
        let nl = hay[at..].find('\n').map(|i| at + i);
        if need_newline && nl.is_none() {
            return None;
        }
        let line_start = hay[..at].rfind('\n').map_or(0, |i| i + 1);
        let line_end = nl.unwrap_or(hay.len());
        let line = hay[line_start..line_end].trim_end_matches('\r').to_string();
        self.cursor += line_end;
        Some(line)
    }
}

#[cfg(test)]
mod boot_isolation {
    use super::*;

    /// A source that replays chunks and then goes quiet, so a test can spell
    /// out "the previous boot was still talking, then it stopped".
    struct Chunks(std::collections::VecDeque<Vec<u8>>);

    impl Chunks {
        fn new(cs: &[&str]) -> Self {
            Self(cs.iter().map(|c| c.as_bytes().to_vec()).collect())
        }
    }

    impl Source for Chunks {
        fn read_available(&mut self, budget: Duration) -> io::Result<Vec<u8>> {
            match self.0.pop_front() {
                Some(c) => Ok(c),
                // Out of script: silent, and it must actually block for the
                // window or `quiesce` would be measuring nothing.
                None => {
                    std::thread::sleep(budget);
                    Ok(Vec::new())
                }
            }
        }
    }

    const QUIET: Duration = Duration::from_millis(20);
    const BUDGET: Duration = Duration::from_millis(400);

    /// THE bug this exists for. The previous boot's line is still arriving when
    /// the harness attaches; without a barrier a `--until`-style wait matches
    /// it and calls a run that never got there a pass.
    #[test]
    fn a_wait_cannot_match_output_from_before_the_reset() {
        let mut l = Listener::new(Chunks::new(&["[00009.9] wifi: got ip 10.0.0.5\n"]));
        assert!(l.quiesce(QUIET, BUDGET), "the board stops talking, so silence is reachable");
        l.discard_backlog();
        assert_eq!(
            l.wait_for_line("wifi: got ip", Duration::from_millis(60)),
            Outcome::DeadlineExpired,
            "the pre-reset line must be unmatchable after the barrier"
        );
    }

    /// …and the barrier must not eat the boot that follows it.
    #[test]
    fn the_boot_after_the_barrier_is_still_matched() {
        let mut l = Listener::new(Chunks::new(&["[00009.9] wifi: got ip 10.0.0.5\n"]));
        assert!(l.quiesce(QUIET, BUDGET));
        l.discard_backlog();
        l.absorb(b"[00000.4] wifi: got ip 10.0.0.7\n");
        match l.wait_for_line("wifi: got ip", Duration::from_millis(60)) {
            Outcome::Matched(line) => assert!(line.contains("10.0.0.7"), "must be the NEW boot's line: {line}"),
            other => panic!("the post-barrier line must match: {other:?}"),
        }
    }

    /// The bytes stay in the transcript; only waits stop seeing them. A
    /// barrier that destroyed evidence would be the §8.2 failure again.
    #[test]
    fn the_barrier_hides_bytes_from_waits_but_keeps_them_as_evidence() {
        let mut l = Listener::new(Chunks::new(&["old output\n"]));
        assert!(l.quiesce(QUIET, BUDGET));
        l.discard_backlog();
        assert!(String::from_utf8_lossy(l.bytes()).contains("old output"), "the transcript must keep it");
    }

    /// A board that never stops talking cannot have its boots separated, and
    /// that is reported rather than assumed away.
    #[test]
    fn a_board_that_never_goes_quiet_is_reported_not_assumed() {
        /// Never runs out, so silence is genuinely unreachable — a finite
        /// script would simply end and be reported as quiet, which is correct
        /// but tests nothing.
        struct Noisy;
        impl Source for Noisy {
            fn read_available(&mut self, _: Duration) -> io::Result<Vec<u8>> {
                std::thread::sleep(Duration::from_millis(5));
                Ok(b"chatter\n".to_vec())
            }
        }
        let mut l = Listener::new(Noisy);
        assert!(!l.quiesce(QUIET, Duration::from_millis(120)), "silence is not reachable, so this must say so");
    }

    /// Quiescing is not allowed to lose anything it read on the way.
    #[test]
    fn output_read_while_waiting_for_silence_is_kept() {
        let mut l = Listener::new(Chunks::new(&["tail of the old boot\n"]));
        assert!(l.quiesce(QUIET, BUDGET));
        assert!(String::from_utf8_lossy(l.bytes()).contains("tail of the old boot"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted source: each entry is one `read_available` result, so a test
    /// can spell out chunk boundaries and silences exactly.
    struct Scripted {
        chunks: Vec<io::Result<Vec<u8>>>,
        budgets: Vec<Duration>,
    }

    impl Scripted {
        fn new(chunks: Vec<&str>) -> Self {
            Self { chunks: chunks.into_iter().map(|c| Ok(c.as_bytes().to_vec())).collect(), budgets: Vec::new() }
        }
    }

    impl Source for Scripted {
        fn read_available(&mut self, budget: Duration) -> io::Result<Vec<u8>> {
            self.budgets.push(budget);
            if self.chunks.is_empty() {
                std::thread::sleep(budget.min(Duration::from_millis(60)));
                return Ok(Vec::new());
            }
            self.chunks.remove(0)
        }
    }

    #[test]
    fn a_matching_line_is_returned_and_kept() {
        let mut l = Listener::new(Scripted::new(vec!["demo report: handshake=798ms\n"]));
        match l.wait_for("demo report", Duration::from_secs(1)) {
            Outcome::Matched(line) => assert!(line.contains("798ms"), "{line}"),
            other => panic!("expected a match, got {other:?}"),
        }
        assert_eq!(l.lines().len(), 1);
    }

    /// A serial read lands mid-line routinely. `wait_for_line` must hold out
    /// for the newline and hand back the WHOLE line, or a parse downstream
    /// fails on output that was merely incomplete rather than wrong.
    #[test]
    fn wait_for_line_reassembles_a_line_split_across_reads() {
        let mut l = Listener::new(Scripted::new(vec!["demo re", "port: handsh", "ake=798ms\n"]));
        match l.wait_for_line("demo report", Duration::from_secs(1)) {
            Outcome::Matched(line) => assert_eq!(line, "demo report: handshake=798ms"),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// The counterpart, and why there are two methods: `wait_for` returns the
    /// INSTANT the pattern appears, mid-line, because for a panic the arrival
    /// is the news and the newline may never come.
    #[test]
    fn wait_for_matches_mid_line_without_waiting_for_the_newline() {
        let mut l = Listener::new(Scripted::new(vec!["demo re", "port: handsh", "ake=798ms\n"]));
        match l.wait_for("demo report", Duration::from_secs(1)) {
            Outcome::Matched(line) => assert_eq!(line, "demo report: handsh", "returns what exists so far"),
            other => panic!("expected a match, got {other:?}"),
        }
    }

    /// A partial hit must not consume the bytes: if `wait_for_line` advanced
    /// the cursor past an unterminated match, the completed line could never be
    /// found and the wait would hang until its deadline on data it had seen.
    #[test]
    fn a_partial_hit_does_not_swallow_the_completed_line() {
        let mut l = Listener::new(Scripted::new(vec!["demo report: han", "dshake=798ms\n"]));
        assert!(matches!(l.wait_for_line("demo report", Duration::from_secs(1)), Outcome::Matched(_)));
    }

    /// THE case an assumption of lines destroys: a board that dies mid-sentence
    /// never sends the closing newline, and those bytes are the whole story.
    #[test]
    fn a_board_dying_mid_line_still_leaves_its_last_words() {
        let mut l = Listener::new(Scripted::new(vec!["WiFi up\nGuru Meditation Error: Core 0 pani"]));
        assert_eq!(l.wait_for("demo report", Duration::from_millis(80)), Outcome::DeadlineExpired);
        let raw = String::from_utf8_lossy(l.bytes()).into_owned();
        assert!(raw.contains("Guru Meditation"), "the unterminated tail must survive: {raw:?}");
        assert!(l.lines().iter().any(|s| s.contains("Core 0 pani")), "and be visible as a line");
    }

    /// And it must be findable while still unterminated — waiting for the
    /// newline of a line that will never end is waiting forever.
    #[test]
    fn a_pattern_in_an_unterminated_line_is_matched() {
        let mut l = Listener::new(Scripted::new(vec!["Guru Meditation Error"]));
        assert!(matches!(l.wait_for("Guru Meditation", Duration::from_secs(1)), Outcome::Matched(_)));
    }

    #[test]
    fn crlf_does_not_end_up_inside_a_line() {
        let mut l = Listener::new(Scripted::new(vec!["ready\r\n"]));
        l.wait_for("ready", Duration::from_secs(1));
        assert_eq!(l.lines()[0], "ready");
    }

    /// Lines arriving while we wait for something else are still evidence.
    #[test]
    fn unmatched_output_is_still_evidence() {
        let mut l = Listener::new(Scripted::new(vec!["WiFi up\nSDP request\ndemo report: x\n"]));
        l.wait_for("demo report", Duration::from_secs(1));
        assert_eq!(l.lines().len(), 3);
        assert_eq!(l.lines()[0], "WiFi up");
    }

    /// A failed wait leaves the evidence behind — what makes a retry
    /// diagnosable instead of a mystery.
    #[test]
    fn a_failed_wait_still_leaves_the_bytes() {
        let mut l = Listener::new(Scripted::new(vec!["SDP request\nTCP connect refused\n"]));
        assert_eq!(l.wait_for("demo report", Duration::from_millis(80)), Outcome::DeadlineExpired);
        assert!(l.lines().iter().any(|s| s.contains("refused")));
    }

    /// The false pass a run-long buffer makes possible, and why the cursor
    /// exists: session N's report must not satisfy session N+1's wait.
    #[test]
    fn an_already_consumed_match_cannot_satisfy_a_later_wait() {
        let mut l = Listener::new(Scripted::new(vec!["demo report: first\n"]));
        assert!(matches!(l.wait_for("demo report", Duration::from_millis(80)), Outcome::Matched(_)));
        assert_eq!(
            l.wait_for("demo report", Duration::from_millis(80)),
            Outcome::DeadlineExpired,
            "the previous session's report must not count twice"
        );
        assert_eq!(l.lines().len(), 1, "and it is still evidence");
    }

    /// A reset invalidates the fd but must not cost the run its evidence —
    /// the §8.2 failure a vanishing port produces without truncating anything.
    #[test]
    fn reattaching_after_a_reset_keeps_everything_said_before_it() {
        let mut l = Listener::new(Scripted::new(vec!["session 1 done\n"]));
        assert!(matches!(l.wait_for("session 1", Duration::from_millis(80)), Outcome::Matched(_)));

        l.across_reset(|| Ok::<_, ()>(Scripted::new(vec!["session 2 done\n"]))).expect("reconnect");
        assert!(matches!(l.wait_for("session 2", Duration::from_millis(80)), Outcome::Matched(_)));

        let raw = String::from_utf8_lossy(l.bytes()).into_owned();
        assert!(raw.contains("session 1 done"), "pre-reset evidence must survive: {raw:?}");
        assert!(raw.contains("session 2 done"), "and post-reset output must append: {raw:?}");
        assert_eq!(l.lines().len(), 2);
    }

    /// The cursor belongs to the run, not to the boot. If a reset cleared it,
    /// the session before the reset would satisfy the wait after it — the exact
    /// false pass the cursor exists to prevent, reintroduced by a reboot.
    #[test]
    fn reattaching_does_not_let_a_pre_reset_report_satisfy_a_later_wait() {
        let mut l = Listener::new(Scripted::new(vec!["demo report: first\n"]));
        assert!(matches!(l.wait_for("demo report", Duration::from_millis(80)), Outcome::Matched(_)));

        l.across_reset(|| Ok::<_, ()>(Scripted::new(vec![]))).expect("reconnect");
        assert_eq!(
            l.wait_for("demo report", Duration::from_millis(80)),
            Outcome::DeadlineExpired,
            "the pre-reset report must not count for the post-reset session"
        );
    }

    /// The transcript is the run's, so it must keep receiving across a reset —
    /// otherwise the boot that crashed is the one with no evidence.
    #[test]
    fn the_sink_survives_a_reset() {
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl io::Write for Shared {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("test mutex").extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut l = Listener::new(Scripted::new(vec!["before\n"])).streaming_to(Box::new(Shared(Arc::clone(&seen))));
        l.wait_for("before", Duration::from_millis(80));
        l.across_reset(|| Ok::<_, ()>(Scripted::new(vec!["after\n"]))).expect("reconnect");
        l.wait_for("after", Duration::from_millis(80));

        let written = String::from_utf8_lossy(&seen.lock().expect("test mutex").clone()).into_owned();
        assert!(written.contains("before") && written.contains("after"), "transcript must span the reset: {written:?}");
    }

    /// THE contract of `across_reset`, and the one an ordinary setter cannot
    /// have: the old port is closed **before** the reset runs. A reset tool
    /// cannot open a port this process is holding, so getting this backwards
    /// makes every reset fail — and it would fail in a way that looks like a
    /// flaky board rather than like a bug here.
    #[test]
    fn the_old_port_is_released_before_the_reset_runs() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        struct Tracked(Arc<AtomicBool>);
        impl Source for Tracked {
            fn read_available(&mut self, _b: Duration) -> io::Result<Vec<u8>> {
                Ok(Vec::new())
            }
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let closed = Arc::new(AtomicBool::new(false));
        let mut l = Listener::new(Tracked(Arc::clone(&closed)));
        let seen_inside = Arc::new(AtomicBool::new(false));

        l.across_reset(|| {
            // This closure stands in for "reset the board and wait for it back".
            seen_inside.store(closed.load(Ordering::SeqCst), Ordering::SeqCst);
            Ok::<_, ()>(Tracked(Arc::new(AtomicBool::new(false))))
        })
        .expect("reconnect");

        assert!(seen_inside.load(Ordering::SeqCst), "the old fd must already be closed when the reset runs");
    }

    /// A failed reconnect must not degrade into a silence. If it did, the run
    /// would spend every remaining deadline waiting on a port that no longer
    /// exists and then report a timeout — sending the next reader at the budget
    /// instead of at the reset that failed.
    #[test]
    fn a_failed_reconnect_reports_a_source_failure_not_a_timeout() {
        let mut l = Listener::new(Scripted::new(vec!["before the reset\n"]));
        l.wait_for("before", Duration::from_millis(80));

        let e = l.across_reset(|| Err::<Scripted, _>("the board never came back")).expect_err("must propagate");
        assert_eq!(e, "the board never came back");

        let t0 = Instant::now();
        match l.wait_for("anything", Duration::from_secs(5)) {
            Outcome::SourceFailed(m) => assert!(m.contains("no source"), "must say the port is gone: {m}"),
            other => panic!("expected SourceFailed, got {other:?}"),
        }
        assert!(t0.elapsed() < Duration::from_millis(200), "must not sit out the deadline: {:?}", t0.elapsed());
        assert!(
            String::from_utf8_lossy(l.bytes()).contains("before the reset"),
            "and the evidence from before the failed reset must survive"
        );
    }

    /// A source error is not a silence. Waiting longer cannot help a dead port.
    #[test]
    fn a_source_error_is_distinguished_from_silence() {
        let mut l =
            Listener::new(Scripted { chunks: vec![Err(io::Error::other("port vanished"))], budgets: Vec::new() });
        match l.wait_for("anything", Duration::from_secs(5)) {
            Outcome::SourceFailed(m) => assert!(m.contains("vanished"), "{m}"),
            other => panic!("expected SourceFailed, got {other:?}"),
        }
    }

    #[test]
    fn the_deadline_is_enforced_when_nothing_arrives() {
        let mut l = Listener::new(Scripted::new(vec![]));
        let t0 = Instant::now();
        assert_eq!(l.wait_for("never", Duration::from_millis(150)), Outcome::DeadlineExpired);
        assert!(t0.elapsed() >= Duration::from_millis(150), "must not return early");
        assert!(t0.elapsed() < Duration::from_secs(3), "and must not overrun");
    }

    /// The budget bounds the whole wait, not each read — §8.5's failure mode is
    /// a bounded run quietly becoming unbounded.
    #[test]
    fn a_chatty_source_cannot_extend_the_deadline() {
        let mut l = Listener::new(Scripted::new(vec!["noise\n"; 500]));
        let t0 = Instant::now();
        assert_eq!(l.wait_for("perf", Duration::from_millis(150)), Outcome::DeadlineExpired);
        assert!(t0.elapsed() < Duration::from_secs(2), "bounded despite traffic");
        assert!(!l.lines().is_empty(), "and the noise is still evidence");
    }

    /// §8.4: bytes must reach the transcript AS THEY ARRIVE. A capture written
    /// once at the end is lost to a crash, a SIGTERM or an early return — and a
    /// run that died is exactly the run whose transcript is worth having. This
    /// asserts the sink has the bytes BEFORE the wait that would have written
    /// them at the end has even returned.
    #[test]
    fn bytes_reach_the_sink_as_they_arrive_not_at_the_end() {
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl io::Write for Shared {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                self.0.lock().expect("test mutex").extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut l = Listener::new(Scripted::new(vec!["early noise\n", "demo report: x\n"]))
            .streaming_to(Box::new(Shared(Arc::clone(&seen))));

        // A wait that FAILS: the pattern never appears, so nothing is returned
        // and an end-of-run write would never happen.
        l.wait_for_line("never appears", Duration::from_millis(120));

        let written = seen.lock().expect("test mutex").clone();
        assert!(
            String::from_utf8_lossy(&written).contains("early noise"),
            "the sink must already hold what arrived during a FAILED wait: {:?}",
            String::from_utf8_lossy(&written)
        );
    }

    #[test]
    fn the_remaining_budget_shrinks_across_reads() {
        let mut l = Listener::new(Scripted::new(vec!["a\n", "b\n", "c\n"]));
        l.wait_for("never", Duration::from_millis(200));
        let b = &l.source.as_ref().expect("source attached").budgets;
        assert!(b.len() >= 2, "expected several reads, got {}", b.len());
        assert!(b[1] < b[0], "budget must shrink: {:?} then {:?}", b[0], b[1]);
    }

    /// Non-UTF8 noise on the wire (line noise, a half-flashed board) must not
    /// panic or discard the surrounding evidence.
    #[test]
    fn invalid_utf8_does_not_lose_the_run() {
        let mut l = Listener::new(Scripted {
            chunks: vec![Ok(vec![0xff, 0xfe, b'\n']), Ok(b"demo report: ok\n".to_vec())],
            budgets: Vec::new(),
        });
        assert!(matches!(l.wait_for("demo report", Duration::from_secs(1)), Outcome::Matched(_)));
        assert_eq!(l.bytes()[0], 0xff, "the raw bytes are preserved verbatim");
    }
}
