// SPDX-License-Identifier: MIT OR Apache-2.0
//! Turning one line of firmware output into numbers.
//!
//! Boundary: **a log line in, a typed record out.** Deciding what the numbers
//! *mean* fails that sentence and lives in [`crate::gate`].
//!
//! Optional and structurally so: nothing else in this crate depends on it.
//!
//! The contract [`Report`] asks for is **total** — every field mandatory, a
//! line yielding fewer an error rather than a partial record. A forgiving
//! parser reports a broken run as a healthy one, and a pattern that silently
//! matches nothing returns an empty set, which reads as "no problem found".
//!
//! The primitives locate a value by the key beside it rather than by offset: a
//! positional split shifts silently when a field is added mid-line, and reports
//! confident wrong numbers instead of failing.

use core::fmt;

/// Why a line could not be read as a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The line is not a report at all — the marker is absent.
    NotAReport,
    /// A mandatory field was absent. Carries the key that was looked for.
    MissingField(&'static str),
    /// A field was present but its digits did not parse.
    BadNumber(&'static str),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NotAReport => f.write_str("not a report line"),
            ParseError::MissingField(k) => write!(f, "missing field: {k}"),
            ParseError::BadNumber(k) => write!(f, "field is not a number: {k}"),
        }
    }
}

impl core::error::Error for ParseError {}

/// One record's worth of firmware output.
///
/// The implementor owns both halves that are domain knowledge: which lines are
/// reports ([`Report::MARKER`]) and what a report contains ([`Report::parse`]).
/// This crate supplies only the discipline — all-or-nothing parsing — and the
/// primitives below.
pub trait Report: Sized {
    /// The substring identifying one of these lines. Used by a driver to know
    /// what to wait for, so it is stated once here rather than at each call
    /// site where it could drift from the parse.
    const MARKER: &'static str;

    /// Parse one line.
    ///
    /// # Errors
    /// [`ParseError::NotAReport`] if [`Report::MARKER`] is absent, and
    /// [`ParseError::MissingField`] / [`ParseError::BadNumber`] if any
    /// mandatory field is absent or unreadable. There is no partial success —
    /// see the module docs.
    fn parse(line: &str) -> Result<Self, ParseError>;

    /// Does this line claim to be a report?
    ///
    /// A cheap pre-filter for scanning a whole capture. `true` does not promise
    /// [`Report::parse`] will succeed, and that distinction is the point: a
    /// truncated line marks but does not parse, and is reported as an error
    /// rather than skipped as noise.
    #[must_use]
    fn is_report(line: &str) -> bool {
        line.contains(Self::MARKER)
    }
}

/// Read the number that follows `key`.
///
/// `key` is matched literally and the digits immediately after it are taken.
/// The unit suffix is not consumed, so a caller passes `"handshake="` and the
/// trailing `ms` is simply not part of the digit run.
///
/// # Errors
/// [`ParseError::MissingField`] if `key` is absent, [`ParseError::BadNumber`]
/// if no digits follow it.
pub fn number_after(hay: &str, key: &'static str) -> Result<u32, ParseError> {
    let rest = hay.split_once(key).ok_or(ParseError::MissingField(key))?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().map_err(|_| ParseError::BadNumber(key))
}

/// Read the number that *precedes* `key`, scanning back over digits.
///
/// Needed because firmware routinely states a value before its unit-and-label
/// (`494ms APP-EC offload`, `longest block 8ms`). Matching forwards on those
/// would need the label, which differs per phase.
///
/// # Errors
/// As [`number_after`].
pub fn number_before(hay: &str, key: &'static str) -> Result<u32, ParseError> {
    let head = hay.split_once(key).ok_or(ParseError::MissingField(key))?.0;
    let digits: String =
        head.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().map_err(|_| ParseError::BadNumber(key))
}

/// Read the digit run at the very start of `hay`.
///
/// For a value whose key has already been split off, so what remains begins
/// with the digits. `key` is carried only for the error message — it names the
/// field the caller was reading, so a failure says which one rather than
/// "position 0".
///
/// # Errors
/// [`ParseError::BadNumber`] if `hay` does not begin with digits.
pub fn leading_number(hay: &str, key: &'static str) -> Result<u32, ParseError> {
    let digits: String = hay.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().map_err(|_| ParseError::BadNumber(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report shaped like a real one, to exercise the primitives together.
    /// Deliberately not this crate's format — there isn't one; the point is
    /// that a consumer's arbitrary shape is expressible.
    #[derive(Debug, PartialEq, Eq)]
    #[allow(clippy::struct_field_names, reason = "the shared `_ms` suffix is the unit, and is the point")]
    struct Sample {
        handshake_ms: u32,
        offload_ms: u32,
        session_ms: u32,
        total_ms: u32,
    }

    impl Report for Sample {
        const MARKER: &'static str = "perf:";
        fn parse(line: &str) -> Result<Self, ParseError> {
            let body = line.split_once(Self::MARKER).ok_or(ParseError::NotAReport)?.1;
            let (head, tail) = body.split_once(" session=").ok_or(ParseError::MissingField(" session="))?;
            Ok(Sample {
                handshake_ms: number_after(head, "handshake=")?,
                offload_ms: number_before(head, "ms offload")?,
                session_ms: leading_number(tail, " session=")?,
                total_ms: number_after(tail, "total=")?,
            })
        }
    }

    const LINE: &str = "[00006.377 INFO ] perf: handshake=798ms (494ms offload) session=1812ms total=2610ms";

    #[test]
    fn every_field_of_a_line_is_read() {
        let p = Sample::parse(LINE).expect("must parse");
        assert_eq!(p, Sample { handshake_ms: 798, offload_ms: 494, session_ms: 1812, total_ms: 2610 });
    }

    #[test]
    fn the_console_prefix_is_optional() {
        let bare = LINE.split_once("] ").expect("has a prefix").1;
        assert_eq!(Sample::parse(bare), Sample::parse(LINE));
    }

    #[test]
    fn a_non_report_line_is_rejected_not_silently_skipped() {
        assert_eq!(Sample::parse("session 4 OK"), Err(ParseError::NotAReport));
        assert!(!Sample::is_report("session 4 OK"));
        assert!(Sample::is_report(LINE));
    }

    /// THE case a forgiving parser gets wrong: a truncated line MARKS as a
    /// report but must not parse, or the fields that survived are reported as a
    /// healthy run.
    #[test]
    fn a_truncated_line_fails_rather_than_returning_a_partial_record() {
        let cut = &LINE[..LINE.len() - 20];
        assert!(Sample::is_report(cut), "still looks like a report");
        assert!(Sample::parse(cut).is_err(), "but must not yield a record");
    }

    #[test]
    fn a_missing_field_names_itself() {
        let gone = LINE.replace(" total=2610ms", "");
        assert_eq!(Sample::parse(&gone), Err(ParseError::MissingField("total=")));
    }

    #[test]
    fn a_non_numeric_field_is_an_error_not_a_zero() {
        let bad = LINE.replace("handshake=798ms", "handshake=???ms");
        assert_eq!(Sample::parse(&bad), Err(ParseError::BadNumber("handshake=")));
    }

    /// Zero is a legitimate value and must not be confused with absence — the
    /// distinction `Option`-flavoured parsing loses.
    #[test]
    fn zero_parses_as_zero() {
        let z = LINE.replace("total=2610ms", "total=0ms");
        assert_eq!(Sample::parse(&z).expect("parses").total_ms, 0);
    }

    /// The parse is position-independent, so a field inserted in the middle
    /// does not shift the values after it — the defect a fixed-offset split
    /// has.
    #[test]
    fn a_new_field_in_the_middle_does_not_shift_the_others() {
        let grown = LINE.replace(" session=", " retries=2 session=");
        assert_eq!(Sample::parse(&grown), Sample::parse(LINE));
    }

    #[test]
    fn number_before_scans_back_over_digits_only() {
        assert_eq!(number_before("(494ms offload)", "ms offload"), Ok(494));
        assert_eq!(number_before("(ms offload)", "ms offload"), Err(ParseError::BadNumber("ms offload")));
        assert_eq!(number_before("nothing here", "ms offload"), Err(ParseError::MissingField("ms offload")));
    }
}
