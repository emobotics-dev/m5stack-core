// SPDX-License-Identifier: MIT OR Apache-2.0
//! `m5stack-core-hil` — a hardware-in-the-loop harness for M5Stack boards:
//! claim a board, drive it, preserve its evidence, judge the result.
//!
//! A **host** tool (`std`, Linux), specific to no application. Dependencies are
//! confined to parsing a command line and a config file; the run logic takes
//! none, so its tests are instant. The manifest says where that line falls.
//!
//! It exists rather than a shell script because a runner's failures are
//! structural: no guaranteed release leaks a tty and makes a live board look
//! dead, truncate-then-retry destroys the evidence for the failure it is
//! recovering from, and knowledge that lives in prose has no testable unit.
//! [`Drop`] and an append-only capture make the first two guarantees rather
//! than reminders; separating decisions from I/O makes the third testable.
//!
//! ## Module boundaries
//!
//! Stated so each file obviously passes or fails on sight.
//!
//! - [`wait`] — an unobservable condition as a bounded wait that says why it
//!   gave up. Knowing what the condition is fails this.
//! - [`lock`] — an exclusive claim on a named resource, and its release.
//! - [`serial`] — getting bytes off a tty.
//! - [`listen`] — bytes accumulated for a run, and the decision to stop waiting.
//! - [`identity`] — bytes into an identity, from a console line or an ELF.
//! - [`config`] — a config file in, board definitions out. A MAC is a fact
//!   about a *rig*, so it lives in `hil.toml` and the tooling takes a name.
//! - [`board`] — the board as a thing that can be named, restarted and asked
//!   what it is.
//! - [`flash`] — deciding whether this board needs this image, and proving the
//!   write took.
//!
//! [`report`] and [`gate`] are optional and nothing else depends on them: they
//! are for consumers wanting a *measured* run judged. What a run measures, and
//! what counts as too slow, stay with the domain that knows.

pub mod board;
pub mod config;
pub mod flash;
pub mod gate;
pub mod identity;
pub mod listen;
pub mod lock;
pub mod report;
pub mod serial;
pub mod wait;
