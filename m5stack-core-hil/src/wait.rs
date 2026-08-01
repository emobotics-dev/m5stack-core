// SPDX-License-Identifier: MIT OR Apache-2.0
//! The one wait that has nothing to block on: bounded, loud, and named.
//!
//! Boundary: **an unobservable condition as a bounded wait that reports why it
//! gave up.** Knowing what the condition *is* fails that sentence.
//!
//! `conventions/testing.md` §1 forbids poll loops, and every other wait here
//! obeys it by blocking in the kernel on the port. Exactly one condition has no
//! such event: **a USB CDC node reappearing after a reset**, where the fd is
//! invalid and the file to block on does not exist yet. §1's ladder says to
//! make it observable — `inotify`/`udev` would, at the cost of a dependency
//! this crate's run logic deliberately does without. Same trade, same escape
//! hatch: take it when this needs to be event-driven for its own sake.
//!
//! Unlike a blind `sleep`, [`until`] returns the instant the condition holds,
//! and on expiry names the condition and the last reason it failed.

use std::{
    thread,
    time::{Duration, Instant},
};

/// Retry `attempt` until it succeeds or `budget` elapses, sleeping `gap`
/// between tries.
///
/// `what` names the condition and appears in the failure, because a bare
/// "timed out" is the message that costs an hour. On expiry the error is
/// `waiting for <what>: gave up after <elapsed> (last: <reason>)`.
///
/// `attempt` is called **before** any sleeping, so a condition that is already
/// true costs nothing — the property a fixed delay cannot have.
///
/// # Errors
/// The budget elapsed without a success. The message carries the last failure
/// reason, so the caller never has to guess which half went wrong.
pub fn until<T>(
    what: &str,
    budget: Duration,
    gap: Duration,
    mut attempt: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let start = Instant::now();
    // No initialiser: the only path that reads this goes through the `Err` arm
    // below, so the compiler proves it is set. A placeholder here would be dead
    // and clippy says so.
    let mut last;
    loop {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(e) => last = e,
        }
        // Checked AFTER an attempt, never before: a budget of zero must still
        // get one honest look at the condition, or a caller who happens to ask
        // at the deadline is told "no" about something that was true.
        let elapsed = start.elapsed();
        if elapsed >= budget {
            return Err(format!("waiting for {what}: gave up after {elapsed:?} (last: {last})"));
        }
        // Never overshoot the budget: sleeping a full `gap` at the deadline
        // turns a bounded wait into a slightly-longer one, which is how a
        // measured ceiling stops being the ceiling.
        thread::sleep(gap.min(budget.saturating_sub(elapsed)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_condition_already_true_returns_at_once_and_never_sleeps() {
        let t0 = Instant::now();
        let r = until("nothing", Duration::from_secs(30), Duration::from_secs(5), || Ok(7));
        assert_eq!(r, Ok(7));
        assert!(t0.elapsed() < Duration::from_millis(50), "must not sleep at all: {:?}", t0.elapsed());
    }

    #[test]
    fn it_retries_until_the_condition_becomes_true() {
        let mut tries = 0;
        let r = until("the third try", Duration::from_secs(5), Duration::from_millis(5), || {
            tries += 1;
            if tries < 3 { Err(format!("try {tries}")) } else { Ok(tries) }
        });
        assert_eq!(r, Ok(3));
    }

    /// The difference from a blind delay that matters most: on expiry the
    /// caller is told WHICH condition failed and WHY, not merely that time
    /// passed. A bare "timed out" is the message that costs an hour.
    #[test]
    fn expiry_names_the_condition_and_carries_the_last_reason() {
        let e = until("the port to reappear", Duration::from_millis(30), Duration::from_millis(5), || {
            Err::<(), _>("no such device".to_string())
        })
        .expect_err("must fail");
        assert!(e.contains("the port to reappear"), "must name the condition: {e}");
        assert!(e.contains("no such device"), "must carry the last reason: {e}");
    }

    #[test]
    fn the_budget_is_honoured_and_not_overshot_by_the_gap() {
        let t0 = Instant::now();
        // A gap far larger than the budget: a naive loop would sleep the whole
        // gap once and blow the ceiling by 10x.
        let _ = until("never", Duration::from_millis(60), Duration::from_secs(2), || Err::<(), _>("no".into()));
        let e = t0.elapsed();
        assert!(e >= Duration::from_millis(60), "must not return early: {e:?}");
        assert!(e < Duration::from_millis(600), "must not overshoot the budget by the gap: {e:?}");
    }

    /// A zero budget still gets one look. Otherwise a caller asking about a
    /// condition that is already true is told "no", which is a lie that reads
    /// like a timeout.
    #[test]
    fn a_zero_budget_still_attempts_once() {
        let mut tries = 0;
        let _ = until("once", Duration::ZERO, Duration::from_millis(1), || {
            tries += 1;
            Err::<(), _>("no".into())
        });
        assert_eq!(tries, 1, "the condition must be looked at even with no budget");
    }
}
