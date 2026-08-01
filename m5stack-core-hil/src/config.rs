// SPDX-License-Identifier: MIT OR Apache-2.0
//! Which boards this rig has, and how to reach them — kept out of the source.
//!
//! Boundary: **a config file in, board definitions out.** Opening a port fails
//! that sentence ([`crate::serial`]).
//!
//! A board's MAC, and the `/dev/serial/by-id/...` name derived from it, is a
//! fact about **a rig** rather than about this crate: hardware gets swapped and
//! there may be a second rig. Baking either into source or a wrapper script
//! makes every such change a code change — the per-run editing this harness
//! exists to end. So boards are named here and the tooling takes a name.
//!
//! Every table is `deny_unknown_fields`: a typo **fails the run naming the
//! line** rather than leaving a setting at a default its author thought they
//! had overridden.
//!
//! ```toml
//! default_rig = "bench"
//!
//! [rigs.bench.cores3]
//! mac    = "1C:DB:D4:BA:83:38"   # the by-id path is derived from this
//! banner = "m5stack-core"        # optional: buys the did-it-boot distinction
//!
//! [rigs.bench.fire27]            # behind a bridge: name the port, and give
//! port = "/dev/serial/by-id/usb-Silicon_Labs_CP2104_5864000922-if00-port0"
//! id   = "fire27-5864000922"     # it a stable id of its own for the lock
//! baud = 1000000
//! chip = "esp32"
//! ```

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{
    board::{Board, Reset},
    flash::FlashConfig,
};

/// The file name looked for when none is given.
pub const FILE_NAME: &str = "hil.toml";

/// The environment variable naming a config file explicitly.
pub const ENV_VAR: &str = "M5STACK_HIL_CONFIG";

/// One board, as the config describes it.
///
/// Every field is optional because a board is addressed one of two ways, and
/// because the flash settings only matter to callers that flash.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardConfig {
    /// A CoreS3's MAC. The `by-id` path is derived from it, so it is not
    /// restated.
    pub mac: Option<String>,
    /// An explicit device path, for a board whose `by-id` name this crate does
    /// not construct.
    pub port: Option<String>,
    /// The stable lock key. Required with `port`; derived from `mac` otherwise.
    pub id: Option<String>,
    pub baud: Option<u32>,
    /// A substring the application prints before its identity. Optional, and
    /// what it buys is the "never reached the application" distinction — see
    /// [`crate::board::read_identity`].
    pub banner: Option<String>,
    /// `"probe-rs"` (over JTAG) or `"espflash"` (over the tty's control lines).
    /// Defaults to probe-rs for a `mac` board and espflash for a `port` one —
    /// a board named by MAC is a CoreS3 with a debug probe, and one named by
    /// port generally is not.
    pub reset: Option<String>,
    /// Which debug probe, as `VID:PID:Serial`. Only needed when `reset =
    /// "probe-rs"` is set on a board whose probe is not derivable from a `mac`
    /// — a rig with several probes must say which, or a reset can hit the
    /// wrong board.
    pub probe: Option<String>,
    pub chip: Option<String>,
    pub flash_size: Option<String>,
    pub flash_freq: Option<String>,
    pub partition_table: Option<String>,
}

impl BoardConfig {
    /// Turn this into an addressable board.
    ///
    /// # Errors
    /// If neither `mac` nor `port` is given, if both are, or if `port` is given
    /// without `id` — a lock keyed to a tty path would let two runs claim one
    /// board under two names, so the stable id is required rather than
    /// invented.
    pub fn to_board(&self) -> Result<Board, String> {
        let mut b = match (&self.mac, &self.port) {
            (Some(mac), None) => Board::cores3(mac),
            (None, Some(port)) => {
                let id = self.id.as_ref().ok_or_else(|| {
                    "a board given by `port` also needs `id` (a stable key for the lock — never a tty index)"
                        .to_string()
                })?;
                Board::at_port(id, port, 1_000_000)
            }
            (Some(_), Some(_)) => return Err("`mac` and `port` are alternatives — give one".into()),
            (None, None) => return Err("a board needs either `mac` or `port`".into()),
        };
        if let Some(id) = &self.id {
            b.id.clone_from(id);
        }
        if let Some(n) = self.baud {
            b.baud = n;
        }
        match self.reset.as_deref() {
            None => {}
            Some("probe-rs") => {
                let chip = self.chip.clone().ok_or_else(|| {
                    "`reset = \"probe-rs\"` also needs `chip` (probe-rs addresses a target by \
                     name, e.g. chip = \"esp32s3\")"
                        .to_string()
                })?;
                b.reset = Reset::ProbeRs { chip, probe: self.probe.clone() };
            }
            Some("espflash") => b.reset = Reset::Espflash,
            Some(other) => return Err(format!("unknown `reset` value `{other}`; use \"probe-rs\" or \"espflash\"")),
        }
        Ok(b)
    }

    /// The flash settings for this board, over the CoreS3 defaults.
    #[must_use]
    pub fn to_flash_config(&self) -> FlashConfig {
        let mut c = FlashConfig::cores3();
        if let Some(v) = &self.chip {
            c.chip.clone_from(v);
        }
        if let Some(v) = &self.flash_size {
            c.flash_size = Some(v.clone());
        }
        if let Some(v) = &self.flash_freq {
            c.flash_freq = Some(v.clone());
        }
        if let Some(v) = &self.partition_table {
            c.partition_table = Some(PathBuf::from(v));
        }
        c
    }
}

/// A parsed config: a default rig, and boards grouped by rig.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The rig used when none is named on the command line.
    pub default_rig: Option<String>,
    /// `rigs.<rig>.<board>`.
    #[serde(default)]
    pub rigs: BTreeMap<String, BTreeMap<String, BoardConfig>>,
}

impl Config {
    /// Look a board up by rig and name.
    ///
    /// `rig` falls back to `default_rig`. A miss lists what *is* defined — a
    /// bare "not found" for a typo'd board name is the message that costs five
    /// minutes.
    ///
    /// # Errors
    /// If no rig can be determined, or the board is not defined.
    pub fn board(&self, rig: Option<&str>, name: &str) -> Result<&BoardConfig, String> {
        let rig = rig.or(self.default_rig.as_deref()).ok_or_else(|| {
            format!("no rig given and no `default_rig` in the config; known boards: {}", self.known())
        })?;
        self.rigs
            .get(rig)
            .ok_or_else(|| format!("no rig `{rig}` in the config; known boards: {}", self.known()))?
            .get(name)
            .ok_or_else(|| format!("no board `{rig}.{name}` in the config; known boards: {}", self.known()))
    }

    /// Every board defined, as `rig.board`, for error messages.
    #[must_use]
    pub fn known(&self) -> String {
        let all: Vec<String> =
            self.rigs.iter().flat_map(|(r, bs)| bs.keys().map(move |b| format!("{r}.{b}"))).collect();
        if all.is_empty() { "(none)".into() } else { all.join(", ") }
    }

    /// Find and load a config: `$M5STACK_HIL_CONFIG`, else `hil.toml` in `dir`
    /// or any ancestor of it, else `~/.config/m5stack-hil/hil.toml`.
    ///
    /// Walking up from `dir` is what lets one config at the top of a checkout
    /// serve every directory inside it.
    ///
    /// # Errors
    /// If a file is found but cannot be read or parsed. A config that is simply
    /// absent is `Ok(None)` — not every invocation needs one.
    pub fn discover(dir: &Path) -> Result<Option<(PathBuf, Self)>, String> {
        // Named explicitly and missing is an error, never a silent fallback to
        // some other file: the caller said which one they meant.
        if let Ok(explicit) = std::env::var(ENV_VAR) {
            let p = PathBuf::from(&explicit);
            let text = std::fs::read_to_string(&p).map_err(|e| format!("{ENV_VAR}={explicit}: {e}"))?;
            return Ok(Some((p.clone(), Self::parse(&text).map_err(|e| format!("{}: {e}", p.display()))?)));
        }
        let mut candidates: Vec<PathBuf> = dir.ancestors().map(|b| b.join(FILE_NAME)).collect();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".config/m5stack-hil").join(FILE_NAME));
        }
        for p in candidates {
            if p.is_file() {
                let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                return Ok(Some((p.clone(), Self::parse(&text).map_err(|e| format!("{}: {e}", p.display()))?)));
            }
        }
        Ok(None)
    }

    /// Parse a config.
    ///
    /// # Errors
    /// On malformed TOML, or on any key this crate does not recognise — see the
    /// module docs on why an unknown key is an error rather than noise.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# the rig this checkout talks to
default_rig = "bench"

[rigs.bench.cores3]
mac    = "1C:DB:D4:BA:83:38"
banner = "m5stack-core"

[rigs.bench.fire27]
port = "/dev/serial/by-id/usb-Silicon_Labs_CP2104_5864000922-if00-port0"
id   = "fire27-5864000922"
baud = 1000000
chip = "esp32"

[rigs.lab2.cores3]
mac = "AA:BB:CC:DD:EE:FF"
"#;

    #[test]
    fn a_board_is_found_by_rig_and_name() {
        let c = Config::parse(SAMPLE).expect("parses");
        let b = c.board(Some("bench"), "cores3").expect("defined");
        assert_eq!(b.mac.as_deref(), Some("1C:DB:D4:BA:83:38"));
        assert_eq!(b.banner.as_deref(), Some("m5stack-core"));
    }

    #[test]
    fn the_default_rig_is_used_when_none_is_named() {
        let c = Config::parse(SAMPLE).expect("parses");
        assert_eq!(c.board(None, "cores3").expect("defined").mac.as_deref(), Some("1C:DB:D4:BA:83:38"));
    }

    /// More than one rig is the point — the same board name resolves
    /// differently per rig, so a second bench needs no code change.
    #[test]
    fn two_rigs_can_define_the_same_board_name() {
        let c = Config::parse(SAMPLE).expect("parses");
        let a = c.board(Some("bench"), "cores3").expect("defined").mac.clone();
        let b = c.board(Some("lab2"), "cores3").expect("defined").mac.clone();
        assert_ne!(a, b);
    }

    /// A typo must say what IS defined. A bare "not found" is the message that
    /// costs five minutes.
    #[test]
    fn an_unknown_board_lists_the_known_ones() {
        let c = Config::parse(SAMPLE).expect("parses");
        let e = c.board(Some("bench"), "cores4").expect_err("not defined");
        assert!(e.contains("bench.cores3"), "must list what exists: {e}");
        assert!(e.contains("bench.fire27"), "{e}");
        assert!(e.contains("lab2.cores3"), "{e}");
    }

    #[test]
    fn an_unknown_rig_is_named_too() {
        let c = Config::parse(SAMPLE).expect("parses");
        assert!(c.board(Some("nope"), "cores3").expect_err("no rig").contains("nope"));
    }

    #[test]
    fn a_mac_board_becomes_a_by_id_path() {
        let c = Config::parse(SAMPLE).expect("parses");
        let b = c.board(None, "cores3").expect("defined").to_board().expect("addressable");
        assert!(b.port.contains("1C:DB:D4:BA:83:38"), "{}", b.port);
        assert_eq!(b.id, "1C:DB:D4:BA:83:38");
        assert!(!b.port.contains("ttyACM"), "must not be a renumbering index: {}", b.port);
    }

    #[test]
    fn an_explicit_port_board_keeps_its_own_id_and_baud() {
        let c = Config::parse(SAMPLE).expect("parses");
        let b = c.board(None, "fire27").expect("defined").to_board().expect("addressable");
        assert_eq!(b.id, "fire27-5864000922");
        assert_eq!(b.baud, 1_000_000);
        assert!(b.port.contains("CP2104"), "{}", b.port);
    }

    #[test]
    fn per_board_flash_settings_override_the_defaults() {
        let c = Config::parse(SAMPLE).expect("parses");
        assert_eq!(c.board(None, "fire27").expect("defined").to_flash_config().chip, "esp32");
        // …and a board that says nothing keeps the CoreS3 default, including the
        // 80 MHz flash clock espflash would otherwise set to 40.
        let s3 = c.board(None, "cores3").expect("defined").to_flash_config();
        assert_eq!(s3.chip, "esp32s3");
        assert_eq!(s3.flash_freq.as_deref(), Some("80mhz"));
    }

    /// A port with no id would have to be locked by its tty path, and a lock
    /// keyed to a tty index lets two runs claim one board under two names.
    #[test]
    fn a_port_without_an_id_is_refused_rather_than_invented() {
        let c = Config::parse("[rigs.r.b]\nport = \"/dev/ttyACM0\"\n").expect("parses");
        let e = c.board(Some("r"), "b").expect("defined").to_board().expect_err("must refuse");
        assert!(e.contains("id"), "{e}");
    }

    /// A CoreS3 has a real debug probe on its USB-Serial-JTAG, so it resets
    /// through that by default rather than through the tty's control lines.
    #[test]
    fn a_mac_board_resets_over_the_probe_and_a_port_board_over_the_serial_lines() {
        let c = Config::parse(SAMPLE).expect("parses");
        let s3 = c.board(None, "cores3").expect("defined").to_board().expect("addressable");
        // The probe is NAMED, not left to probe-rs to choose. On a rig with two
        // ESP JTAG probes an unqualified reset can restart the other board.
        assert_eq!(
            s3.reset,
            Reset::ProbeRs { chip: "esp32s3".into(), probe: Some("303a:1001:1C:DB:D4:BA:83:38".into()) }
        );
        let f27 = c.board(None, "fire27").expect("defined").to_board().expect("addressable");
        assert_eq!(f27.reset, Reset::Espflash);
    }

    #[test]
    fn the_reset_route_can_be_overridden() {
        let c = Config::parse("[rigs.r.b]\nmac = \"m\"\nreset = \"espflash\"\n").expect("parses");
        assert_eq!(c.board(Some("r"), "b").expect("defined").to_board().expect("ok").reset, Reset::Espflash);
    }

    /// probe-rs addresses a target by name, so asking for it without saying
    /// which chip is refused rather than guessed at.
    #[test]
    fn probe_rs_without_a_chip_is_refused() {
        let c = Config::parse("[rigs.r.b]\nport = \"/dev/x\"\nid = \"i\"\nreset = \"probe-rs\"\n").expect("parses");
        let e = c.board(Some("r"), "b").expect("defined").to_board().expect_err("must refuse");
        assert!(e.contains("chip"), "{e}");
    }

    #[test]
    fn an_unknown_reset_route_is_refused_with_the_alternatives() {
        let c = Config::parse("[rigs.r.b]\nmac = \"m\"\nreset = \"openocd\"\n").expect("parses");
        let e = c.board(Some("r"), "b").expect("defined").to_board().expect_err("must refuse");
        assert!(e.contains("openocd") && e.contains("probe-rs"), "{e}");
    }

    #[test]
    fn a_board_with_neither_mac_nor_port_is_refused() {
        let c = Config::parse("[rigs.r.b]\nbaud = 115200\n").expect("parses");
        assert!(c.board(Some("r"), "b").expect("defined").to_board().is_err());
    }

    #[test]
    fn a_board_with_both_mac_and_port_is_refused_rather_than_one_winning() {
        let c = Config::parse("[rigs.r.b]\nmac = \"m\"\nport = \"/dev/x\"\nid = \"i\"\n").expect("parses");
        assert!(c.board(Some("r"), "b").expect("defined").to_board().is_err());
    }

    /// THE property that makes a config safe to edit: a key nobody recognises
    /// is an error, not something to skip. A parser that ignored a typo would
    /// run with settings the author believed were in effect.
    #[test]
    fn an_unknown_board_key_is_refused_not_ignored() {
        let e = Config::parse("[rigs.r.b]\nmac = \"x\"\nflash_frequency = \"80mhz\"\n").expect_err("must refuse");
        assert!(e.contains("flash_frequency"), "must name the offending key: {e}");
    }

    #[test]
    fn an_unknown_top_level_key_is_refused_too() {
        let e = Config::parse("defalt_rig = \"bench\"\n").expect_err("must refuse");
        assert!(e.contains("defalt_rig"), "must name the offending key: {e}");
    }

    #[test]
    fn a_wrongly_typed_value_is_refused() {
        assert!(Config::parse("[rigs.r.b]\nbaud = \"fast\"\n").is_err());
        assert!(Config::parse("default_rig = 7\n").is_err());
    }

    #[test]
    fn an_empty_config_is_valid_and_says_so() {
        let c = Config::parse("").expect("parses");
        assert_eq!(c.known(), "(none)");
        assert!(c.board(None, "cores3").expect_err("no rig").contains("default_rig"));
    }

    /// Real TOML, not a subset — the reason this took a dependency. An author
    /// reasonably expects these to work, and a hand-rolled parser would have
    /// rejected every one of them.
    #[test]
    fn ordinary_toml_spellings_are_accepted() {
        let c = Config::parse(
            "default_rig = 'bench'\n\
             [rigs.bench.cores3]\n\
             mac = '1C:DB:D4:BA:83:38'\n\
             banner = \"\"\"multi\nline\"\"\"\n\
             id = \"esc\\\"aped\"\n",
        )
        .expect("real TOML must parse");
        let b = c.board(None, "cores3").expect("defined");
        assert_eq!(b.mac.as_deref(), Some("1C:DB:D4:BA:83:38"));
        assert_eq!(b.banner.as_deref(), Some("multi\nline"));
        assert_eq!(b.id.as_deref(), Some("esc\"aped"));
    }

    /// The inline-table spelling of the same thing, which authors do reach for.
    #[test]
    fn an_inline_table_board_is_accepted() {
        let c = Config::parse("[rigs.bench]\ncores3 = { mac = \"AA:BB\" }\n").expect("parses");
        assert_eq!(c.board(Some("bench"), "cores3").expect("defined").mac.as_deref(), Some("AA:BB"));
    }
}
