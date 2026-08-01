// SPDX-License-Identifier: MIT OR Apache-2.0
//! Deciding whether a run passed, against a budget declared **before** it ran.
//!
//! Boundary: **records in, a verdict out.** Reading a log line fails that
//! sentence ([`crate::report`]).
//!
//! Optional and structurally so: nothing else in this crate depends on it.
//!
//! Three rules hold for any measured run, and this module owns them so they
//! cannot be forgotten:
//!
//! - **An incomplete run FAILS**, before any number is looked at — the error
//!   may be in the output that never arrived.
//! - **The budget is declared up front**, so a pass cannot be decided after
//!   seeing the numbers. That is the difference from a rationalisation.
//! - **Retries are carried out of band.** Statistics over completed sessions
//!   are silent about the ones that failed, so a clean summary is not evidence
//!   the attempts behind it were healthy.
//!
//! Which metric matters, and what value is tolerable, is domain knowledge:
//! that is [`Gate::check`], and it belongs to the consumer.

/// min / median / max over one metric.
///
/// Median rather than mean: a single outlier in a run should move the reported
/// figure by nothing, and on real hardware outliers are the norm rather than
/// the exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub min: u32,
    pub median: u32,
    pub max: u32,
    pub n: usize,
}

impl Stats {
    /// Summarise `values`.
    ///
    /// `None` for an empty slice, deliberately: there is no meaningful minimum
    /// of nothing, and returning zeros would read as a healthy fast run.
    #[must_use]
    pub fn of(values: &[u32]) -> Option<Self> {
        if values.is_empty() {
            return None;
        }
        let mut v = values.to_vec();
        v.sort_unstable();
        Some(Self { min: v[0], median: v[v.len() / 2], max: v[v.len() - 1], n: v.len() })
    }
}

/// One completed run: what the board said, plus what the harness observed about
/// the run itself.
#[derive(Debug, Clone)]
pub struct Run<R> {
    /// The records that were successfully parsed.
    pub records: Vec<R>,
    /// Session retries the harness spent. **Not** derivable from `records` — a
    /// failed attempt leaves no record, which is the whole point.
    pub retries: u32,
}

impl<R> Run<R> {
    /// A run with no retries — the common case, stated rather than defaulted so
    /// a caller that *did* retry cannot forget to say so.
    #[must_use]
    pub fn clean(records: Vec<R>) -> Self {
        Self { records, retries: 0 }
    }
}

/// What a run must achieve, decided **before** it starts.
///
/// The consumer implements this over its own record type. The two universal
/// budgets are asked for here so [`judge`] can enforce them; everything domain
/// goes in [`Gate::check`].
pub trait Gate {
    /// The record type this gate judges — a [`crate::report::Report`], usually.
    type Record;

    /// How many sessions were requested. Fewer records than this is a FAIL.
    fn sessions(&self) -> usize;

    /// Retries tolerated before the run fails.
    fn max_retries(&self) -> u32;

    /// Domain ceilings.
    ///
    /// Push one message per violated criterion and **never return early**: a
    /// run that broke three rules should say so once, not across three runs.
    /// Called only after the universal checks, and called even when `records`
    /// is short — an incomplete run has already failed, but its numbers may
    /// still be worth reporting on.
    ///
    /// The default does nothing, so a gate that only wants completeness and a
    /// retry budget implements neither this nor a record type of substance.
    fn check(&self, records: &[Self::Record], failures: &mut Vec<String>) {
        let _ = (records, failures);
    }
}

/// The outcome, with every reason it failed rather than the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub passed: bool,
    /// Every violated criterion, in the order checked. Empty iff `passed`.
    pub failures: Vec<String>,
    /// Stated whether or not it caused a failure — see the module docs on why a
    /// retry is never inferred from the statistics.
    pub retries: u32,
    /// How many records the run produced, against how many were requested.
    pub records: usize,
    pub sessions: usize,
}

/// Judge `run` against `gate`.
///
/// Enforces the three universal rules, then defers to [`Gate::check`] for the
/// domain ones.
#[must_use]
pub fn judge<G: Gate>(gate: &G, run: &Run<G::Record>) -> Verdict {
    let mut failures = Vec::new();

    // Completeness first. A short run is a failed run, and its timings must not
    // be allowed to look reassuring before that is said.
    if run.records.len() != gate.sessions() {
        failures.push(format!("incomplete: {} of {} sessions produced a report", run.records.len(), gate.sessions()));
    }

    if run.retries > gate.max_retries() {
        failures.push(format!(
            "{} session retrie(s), budget {} — a retry is invisible in the records and in every \
             statistic computed from them, so it is reported here or nowhere",
            run.retries,
            gate.max_retries()
        ));
    }

    gate.check(&run.records, &mut failures);

    Verdict {
        passed: failures.is_empty(),
        failures,
        retries: run.retries,
        records: run.records.len(),
        sessions: gate.sessions(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A consumer's gate: three sessions, no retries, and one domain ceiling.
    struct Demo {
        sessions: usize,
        max_block_ms: u32,
    }

    impl Gate for Demo {
        type Record = u32; // one metric per session, for the test's purposes

        fn sessions(&self) -> usize {
            self.sessions
        }
        fn max_retries(&self) -> u32 {
            0
        }
        fn check(&self, records: &[u32], failures: &mut Vec<String>) {
            if let Some(s) = Stats::of(records)
                && s.max > self.max_block_ms
            {
                failures.push(format!("longest block {} ms exceeds the {} ms budget", s.max, self.max_block_ms));
            }
        }
    }

    fn gate() -> Demo {
        Demo { sessions: 3, max_block_ms: 10 }
    }

    #[test]
    fn stats_of_nothing_is_none_not_zero() {
        assert_eq!(Stats::of(&[]), None);
    }

    #[test]
    fn median_ignores_a_single_outlier() {
        let s = Stats::of(&[1521, 1719, 2495]).expect("non-empty");
        assert_eq!((s.min, s.median, s.max), (1521, 1719, 2495));
    }

    #[test]
    fn a_clean_run_passes() {
        let v = judge(&gate(), &Run::clean(vec![8, 9, 10]));
        assert!(v.passed, "unexpected failures: {:?}", v.failures);
        assert_eq!((v.records, v.sessions), (3, 3));
    }

    /// A short run FAILS, and does so even though every number it did produce
    /// is well inside budget. This is the case a human eye passes.
    #[test]
    fn a_short_run_fails_even_when_every_number_is_healthy() {
        let v = judge(&gate(), &Run::clean(vec![8, 9]));
        assert!(!v.passed);
        assert!(v.failures.iter().any(|f| f.contains("incomplete: 2 of 3")), "{:?}", v.failures);
    }

    /// A retry is reported even when it broke no ceiling, because it is
    /// invisible in the records and in every statistic computed from them.
    #[test]
    fn a_retry_is_surfaced_and_is_not_inferable_from_the_records() {
        let clean = judge(&gate(), &Run { records: vec![8, 9, 10], retries: 0 });
        let retried = judge(&gate(), &Run { records: vec![8, 9, 10], retries: 1 });
        assert_eq!(clean.records, retried.records, "identical records...");
        assert!(clean.passed, "...but only one passes");
        assert!(!retried.passed);
        assert_eq!(retried.retries, 1);
    }

    /// The domain ceiling is the consumer's, and it is consulted.
    #[test]
    fn a_domain_ceiling_violation_is_caught() {
        let v = judge(&gate(), &Run::clean(vec![8, 14, 10]));
        assert!(!v.passed);
        assert!(v.failures.iter().any(|f| f.contains("14 ms exceeds")), "{:?}", v.failures);
    }

    /// Every violated criterion is reported, not just the first — a run that is
    /// both short and over budget should say so once, not across three runs.
    #[test]
    fn all_failures_are_reported_not_just_the_first() {
        let v = judge(&gate(), &Run { records: vec![20], retries: 2 });
        assert_eq!(v.failures.len(), 3, "{:?}", v.failures);
    }

    /// A gate with no domain rules is legitimate and gets the universal ones
    /// for free — this is what makes the module optional in practice as well as
    /// structurally.
    #[test]
    fn a_gate_with_no_domain_checks_still_enforces_completeness() {
        struct BareMinimum;
        impl Gate for BareMinimum {
            type Record = ();
            fn sessions(&self) -> usize {
                2
            }
            fn max_retries(&self) -> u32 {
                0
            }
        }
        assert!(judge(&BareMinimum, &Run::clean(vec![(), ()])).passed);
        let short = judge(&BareMinimum, &Run::clean(vec![()]));
        assert!(!short.passed);
        assert!(short.failures.iter().any(|f| f.contains("incomplete: 1 of 2")), "{:?}", short.failures);
    }
}
