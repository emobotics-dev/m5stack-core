// SPDX-License-Identifier: MIT OR Apache-2.0
//! What image is on this chip: the firmware's identity, from a console line or
//! from an ELF.
//!
//! Boundary: **turning bytes into an identity.** Getting those bytes off a
//! board fails that sentence, and so does deciding what to do when two
//! identities differ ([`crate::flash`]).
//!
//! ## Why ask the board rather than keep a stamp file
//!
//! A stamp answers *what do we believe we flashed?*, and is wrong where it
//! costs most: a flash that went around the guard leaves it claiming an image
//! the board does not have, so the guard **skips** and the run measures the
//! wrong firmware. It also hashes the ELF, so a half-written image on the chip
//! is invisible to it. Asking the board is immune to both, and cheaper than
//! flashing anyway (~6 s against ~15 s here).
//!
//! ## Mark and hash answer different questions
//!
//! The **mark** is a compile-time literal — package, binary, features, commit —
//! for a person to read; two builds from one commit with different uncommitted
//! edits share it. The **hash** is `app_elf_sha256`, which `espflash` patches
//! in after linking, so it is a function of the bytes actually flashed and is
//! the only half that sees an uncommitted edit. [`Identity`] carries both and
//! compares as one value.
//!
//! No delimiter handling is needed: the mark is the `version` field of the
//! esp-idf application descriptor — a `#[repr(C)]` struct at a fixed offset
//! behind a magic word — so [`from_elf`] is a structured read, not a substring
//! search over `.rodata`.

use core::fmt;

/// The console marker `m5stack-core` prints its identity behind.
///
/// Must stay equal to `m5stack_core::io::console::markers::IDENTITY`. It cannot
/// be imported: that crate is `no_std` and builds only for Xtensa, and this one
/// is a host tool. The pairing is asserted by
/// `tests::the_marker_matches_what_the_bsp_emits` against a captured line.
pub const MARKER: &str = "identity:";

/// The key introducing the crate version on the identity line.
const VERSION_KEY: &str = "version=";
/// The key introducing the content hash on the identity line.
const SHA_KEY: &str = "app_elf_sha256=";

/// The full identity a comparison is made on.
///
/// One value rather than three fields compared separately, so a caller cannot
/// check one half and forget the other — which is the mistake that leaves a
/// guard blind to exactly the case the hash was added for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The descriptor's `version` field: the build mark under `m5stack-core`'s
    /// `identity` feature, or the plain crate version without it.
    pub mark: String,
    /// The crate version the firmware reported alongside the mark.
    pub version: String,
    /// The leading hex of `app_elf_sha256`, lower-case, exactly as the board
    /// printed it. Compared on this prefix — see [`Identity::hash_prefix_len`].
    pub sha256_prefix: String,
}

impl Identity {
    /// Is the hash the board reported the unpatched initialiser?
    ///
    /// The board-side counterpart of [`AppDesc::hash_is_unpatched`] — the same
    /// fact, read off the hex a board printed rather than the bytes in a file.
    /// Both exist because the zero hash shows up on both sides of the
    /// comparison, and a caller that meets it on only one of them writes the
    /// check twice.
    ///
    /// An empty prefix is **not** unpatched: it means the board printed no
    /// hash at all, which is a parse question and not this one.
    #[must_use]
    pub fn hash_is_unpatched(&self) -> bool {
        !self.sha256_prefix.is_empty() && self.sha256_prefix.chars().all(|c| c == '0')
    }

    /// How many hex characters of the content hash a comparison uses.
    ///
    /// Not a constant of this crate: the board decides how much it prints, and
    /// the ELF side truncates to match rather than the other way round. Reading
    /// it off the captured value is what keeps the two sides comparable if
    /// `m5stack-core` ever prints more or fewer.
    #[must_use]
    pub fn hash_prefix_len(&self) -> usize {
        self.sha256_prefix.len()
    }
}

impl fmt::Display for Identity {
    /// The canonical comparable form, and the one worth putting in a message:
    /// it names both halves, so a mismatch report shows *which* half differs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {VERSION_KEY}{} {SHA_KEY}{}", self.mark, self.version, self.sha256_prefix)
    }
}

/// Read an [`Identity`] out of one console line.
///
/// Returns `None` unless **every** part is present: the marker, a mark, a
/// version and a hash. A partial identity is deliberately not half-trusted —
/// firmware that named itself but not comparably (no hash) must count as
/// unidentified, or accepting it silently restores the blindness to uncommitted
/// edits that the hash exists to remove.
///
/// Anything before the marker (a console timestamp, a log level) is ignored, so
/// this reads a raw capture and a stripped one alike.
#[must_use]
pub fn from_line(line: &str) -> Option<Identity> {
    let body = line.split_once(MARKER)?.1.trim_start();
    // The mark is the first whitespace-delimited token. It cannot contain a
    // space: `app_desc!` const-asserts it into a 31-byte C string, and the
    // joined form is `<pkg>/<bin>/<features>/<hash>`.
    let mark = body.split_whitespace().next()?;
    if mark.is_empty() || mark.starts_with(VERSION_KEY) {
        return None;
    }
    let version = value_after(body, VERSION_KEY)?;
    let sha = value_after(body, SHA_KEY)?;
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(Identity { mark: mark.to_string(), version: version.to_string(), sha256_prefix: sha.to_ascii_lowercase() })
}

/// The whitespace-delimited value introduced by `key`, or `None` if absent or
/// empty. Position-independent, so a field added to the middle of the line does
/// not shift the ones after it.
fn value_after<'a>(hay: &'a str, key: &str) -> Option<&'a str> {
    let rest = hay.split_once(key)?.1;
    let val = rest.split_whitespace().next()?;
    if val.is_empty() { None } else { Some(val) }
}

// --- The ELF side: the esp-idf application descriptor -----------------------

/// `ESP_APP_DESC_MAGIC_WORD`, the descriptor's first field.
///
/// Searching for a magic `u32` rather than a string is what makes the ELF read
/// structured: everything else is at a fixed offset from it, so nothing has to
/// guess where a value ends.
const MAGIC: u32 = 0xABCD_5432;

/// Field offsets within `esp_app_desc_t`, which is `#[repr(C)]` and therefore
/// fixed. Mirrors `esp_bootloader_esp_idf::EspAppDesc`; verified against a real
/// ELF, not read off the struct definition alone — see the tests.
const OFF_VERSION: usize = 16;
const OFF_PROJECT_NAME: usize = 48;
const OFF_SHA256: usize = 144;
/// Bytes of descriptor that must be present for the fields above to be
/// readable.
const DESC_MIN_LEN: usize = OFF_SHA256 + 32;
/// Width of the two `c_char` name fields.
const NAME_LEN: usize = 32;

/// The application descriptor as it appears in a linked ELF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDesc {
    /// The `version` field — the build mark under the `identity` feature.
    pub version: String,
    /// The `project_name` field — `CARGO_BIN_NAME` as `app_desc!` records it.
    pub project_name: String,
    /// The `app_elf_sha256` field **as stored in the file**.
    ///
    /// Almost always all-zero: `espflash` computes this over the ELF and
    /// patches it into the image at flash time, so an unflashed ELF carries the
    /// unpatched initialiser. Exposed anyway rather than hidden, because a
    /// caller that assumed otherwise should be able to see that it is zero
    /// instead of silently comparing against nothing — [`crate::flash`] hashes
    /// the file itself for this reason.
    pub app_elf_sha256: [u8; 32],
}

impl AppDesc {
    /// Is the stored hash the unpatched initialiser?
    ///
    /// The distinction that matters to a caller: a zero hash is not a hash, and
    /// comparing it against a board's real one would fail forever.
    #[must_use]
    pub fn hash_is_unpatched(&self) -> bool {
        self.app_elf_sha256 == [0u8; 32]
    }
}

/// Read the application descriptor out of a linked ELF (or any blob containing
/// one).
///
/// Finds the magic word and reads fixed offsets from it. Returns `None` when
/// there is no descriptor, or when the only candidates are too close to the end
/// of the blob for their fields to be complete — a truncated descriptor is not
/// repaired or partially returned.
///
/// Searching a *whole* file is the point: this works on a linked ELF and on an
/// extracted `.flash.appdesc` section alike, so the two cannot disagree about
/// what a descriptor is.
///
/// **A bare magic-word match is not enough.** `0xABCD5432` is four bytes of
/// ordinary-looking data and can occur by chance in `.text` or `.rodata`,
/// potentially *before* the real descriptor — and since any later candidate is
/// nearer the end of the file than an earlier one, a scan that accepted the
/// first match with room could never recover from that. So each candidate is
/// checked for plausibility (a name-field sanity check) and the scan continues
/// past one that fails.
#[must_use]
pub fn from_elf(bytes: &[u8]) -> Option<AppDesc> {
    let magic = MAGIC.to_le_bytes();
    let mut from = 0usize;
    while let Some(rel) = find_sub(&bytes[from..], &magic) {
        let at = from + rel;
        match bytes.get(at..at + DESC_MIN_LEN) {
            Some(d) if looks_like_descriptor(d) => {
                let mut sha = [0u8; 32];
                sha.copy_from_slice(&d[OFF_SHA256..OFF_SHA256 + 32]);
                return Some(AppDesc {
                    version: cstr(&d[OFF_VERSION..OFF_VERSION + NAME_LEN]),
                    project_name: cstr(&d[OFF_PROJECT_NAME..OFF_PROJECT_NAME + NAME_LEN]),
                    app_elf_sha256: sha,
                });
            }
            // Either too close to the end, or a coincidence that is not a
            // descriptor. Keep looking — the real one may be further on.
            _ => from = at + magic.len(),
        }
    }
    None
}

/// Is this candidate plausibly a descriptor rather than a chance magic word?
///
/// Checks the two name fields, which a real descriptor fills with
/// NUL-terminated printable ASCII (`app_desc!` const-asserts the mark into a
/// 31-byte C string, and `project_name` is a crate/binary name). Arbitrary
/// binary rarely satisfies both.
///
/// Deliberately not a checksum: there is none in the struct, so this is a
/// plausibility filter and is documented as one. It exists to stop a
/// coincidence *shadowing* the real descriptor, not to authenticate anything.
fn looks_like_descriptor(d: &[u8]) -> bool {
    [OFF_VERSION, OFF_PROJECT_NAME].iter().all(|&off| {
        let field = &d[off..off + NAME_LEN];
        // A C string field must terminate inside its own width...
        let Some(end) = field.iter().position(|&b| b == 0) else { return false };
        // ...and what precedes the terminator must be printable ASCII.
        field[..end].iter().all(|&b| b.is_ascii_graphic() || b == b' ')
    })
}

/// A fixed-width, NUL-terminated C string field as a `String`.
///
/// Lossy on purpose: the surrounding bytes are arbitrary binary, and a field
/// that is not clean UTF-8 should fail to match the other side rather than
/// abort the comparison.
fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Naive substring search. `std` has no `slice::find`, and this crate takes no
/// dependencies (`Cargo.toml` says why); the inputs are ELFs of a few megabytes
/// searched once per run, which is irrelevant next to the reset it saves.
fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real line, captured from a CoreS3 running the `display` demo. Using
    /// captured output rather than a hand-written sample is deliberate: a
    /// sample invented to match the parser proves only that the parser matches
    /// itself.
    const REAL: &str = "[00000.276 INFO ] identity: demos/display/a30fbf+ version=0.1.0 \
app_elf_sha256=d862a888b3f7";

    #[test]
    fn every_part_of_a_real_line_is_read() {
        let id = from_line(REAL).expect("the captured line must parse");
        assert_eq!(id.mark, "demos/display/a30fbf+");
        assert_eq!(id.version, "0.1.0");
        assert_eq!(id.sha256_prefix, "d862a888b3f7");
        assert_eq!(id.hash_prefix_len(), 12);
    }

    /// The marker is duplicated from `m5stack-core` (a `no_std` Xtensa crate
    /// this host tool cannot depend on), so the one thing that can catch drift
    /// is a captured line.
    #[test]
    fn the_marker_matches_what_the_bsp_emits() {
        assert!(REAL.contains(MARKER), "the BSP's marker must appear in real output: {REAL}");
    }

    /// The console prefix is optional — the same line parses from a capture
    /// that has been stripped of timestamps.
    #[test]
    fn the_console_prefix_is_optional() {
        let bare = REAL.split_once("] ").expect("sample has a prefix").1;
        assert_eq!(from_line(bare), from_line(REAL));
    }

    /// Half an identity must not count as one. Firmware that printed a mark but
    /// no hash named itself in a way that cannot be COMPARED, and accepting it
    /// would silently restore blindness to uncommitted edits.
    #[test]
    fn a_mark_without_a_hash_is_not_an_identity() {
        assert_eq!(from_line("identity: demos/display/a30fbf+ version=0.1.0"), None);
    }

    #[test]
    fn a_line_without_the_marker_is_not_an_identity() {
        assert_eq!(from_line("[00000.100 INFO ] wifi: associated"), None);
        assert_eq!(from_line(""), None);
    }

    /// A non-hex hash is a corrupted capture, not a value to compare against.
    #[test]
    fn a_non_hex_hash_is_refused() {
        let bad = REAL.replace("d862a888b3f7", "not-a-hash!!");
        assert_eq!(from_line(&bad), None);
    }

    /// Two builds from ONE commit with different uncommitted edits share a mark
    /// and differ only in the hash. Comparing marks alone is the blindness the
    /// hash exists to remove, so the values must not compare equal.
    #[test]
    fn identity_distinguishes_two_builds_from_one_dirty_commit() {
        let a = from_line(REAL).expect("parses");
        let b = from_line(&REAL.replace("d862a888b3f7", "0102030405ff")).expect("parses");
        assert_ne!(a, b);
        assert_eq!(a.mark, b.mark, "the marks are identical — that is the point");
    }

    /// …and the feature arm / binary name must distinguish two marks at one
    /// commit, which is what makes the mark worth carrying beside the hash.
    #[test]
    fn the_mark_distinguishes_two_binaries_at_one_commit() {
        let one = from_line(&REAL.replace("demos/display/", "demos/lvgl/")).expect("parses");
        let two = from_line(REAL).expect("parses");
        assert_ne!(one.mark, two.mark);
    }

    #[test]
    fn display_round_trips_through_the_parser() {
        let id = from_line(REAL).expect("parses");
        let rendered = format!("{MARKER} {id}");
        assert_eq!(from_line(&rendered).expect("re-parses"), id);
    }

    // --- the ELF side ------------------------------------------------------

    /// A descriptor built to the layout this module claims. Byte-for-byte the
    /// shape verified against a real `display` ELF's `.flash.appdesc` section
    /// (`objcopy -O binary --only-section=.flash.appdesc`), whose fields read
    /// `version="0.1.0"`, `project_name="display"`, `sha256`=all-zero.
    fn desc(version: &str, project: &str, sha: [u8; 32]) -> Vec<u8> {
        let mut d = vec![0u8; DESC_MIN_LEN + 16];
        d[..4].copy_from_slice(&MAGIC.to_le_bytes());
        d[OFF_VERSION..OFF_VERSION + version.len()].copy_from_slice(version.as_bytes());
        d[OFF_PROJECT_NAME..OFF_PROJECT_NAME + project.len()].copy_from_slice(project.as_bytes());
        d[OFF_SHA256..OFF_SHA256 + 32].copy_from_slice(&sha);
        d
    }

    #[test]
    fn a_descriptor_is_read_at_its_fixed_offsets() {
        let d = desc("demos/display/a30fbf+", "display", [0u8; 32]);
        let got = from_elf(&d).expect("a descriptor must be found");
        assert_eq!(got.version, "demos/display/a30fbf+");
        assert_eq!(got.project_name, "display");
    }

    /// THE reason the ELF side needs no delimiters: the descriptor sits at a
    /// fixed offset behind a magic word, so arbitrary neighbouring bytes cannot
    /// be absorbed into a field the way an adjacent `.rodata` literal can.
    #[test]
    fn neighbouring_binary_junk_is_not_absorbed() {
        let mut blob = vec![0x7f, 0x45, 0x4c, 0x46, 0x01, 0x02];
        blob.extend_from_slice(&desc("0.4.3", "display", [0u8; 32]));
        blob.extend_from_slice(b"crypto: p256 verify=shamir");
        let got = from_elf(&blob).expect("found");
        assert_eq!(got.version, "0.4.3", "the field must stop at its NUL, not run on");
        assert_eq!(got.project_name, "display");
    }

    #[test]
    fn no_descriptor_is_none_not_a_guess() {
        assert_eq!(from_elf(b"nothing to see here"), None);
        assert_eq!(from_elf(b""), None);
    }

    /// A magic word too close to the end cannot yield a complete descriptor.
    /// Without the bound this would read past the blob or return partial
    /// fields.
    #[test]
    fn a_truncated_descriptor_is_refused() {
        let mut d = desc("0.4.3", "display", [0u8; 32]);
        d.truncate(OFF_SHA256 + 4);
        assert_eq!(from_elf(&d), None);
    }

    /// THE case the plausibility check exists for. `0xABCD5432` is four bytes
    /// of ordinary-looking data and can occur by chance in `.text` — and since
    /// every later candidate is nearer the end of the file than an earlier one,
    /// a scan that took the first match with room could never recover from a
    /// coincidence that landed BEFORE the real descriptor.
    #[test]
    fn a_chance_magic_word_does_not_shadow_the_real_descriptor() {
        let mut blob = MAGIC.to_le_bytes().to_vec();
        // Plenty of room after it, so length alone does not reject it — but the
        // name fields never terminate, so it is not a descriptor.
        blob.extend_from_slice(&[0xffu8; DESC_MIN_LEN]);
        blob.extend_from_slice(&desc("0.4.3", "display", [0u8; 32]));
        let got = from_elf(&blob).expect("the real descriptor must still be found");
        assert_eq!(got.version, "0.4.3");
        assert_eq!(got.project_name, "display");
    }

    /// A name field holding binary rather than a C string is not a descriptor.
    #[test]
    fn a_candidate_with_unprintable_names_is_not_a_descriptor() {
        let mut d = desc("0.4.3", "display", [0u8; 32]);
        d[OFF_VERSION] = 0x01; // unprintable, before the NUL
        assert_eq!(from_elf(&d), None);
    }

    /// An empty version is unusual but legitimate, and must not be mistaken for
    /// implausible — the field is simply all-NUL.
    #[test]
    fn an_empty_name_field_is_still_a_descriptor() {
        let d = desc("", "display", [0u8; 32]);
        assert_eq!(from_elf(&d).expect("found").version, "");
    }

    /// The fact that drives `crate::flash`: an unflashed ELF's hash field is
    /// the unpatched initialiser, because `espflash` computes and patches
    /// it at flash time. Verified on a real ELF — its `.flash.appdesc`
    /// reads all-zero.
    #[test]
    fn an_unflashed_elf_reports_its_hash_as_unpatched() {
        let d = desc("0.4.3", "display", [0u8; 32]);
        assert!(from_elf(&d).expect("found").hash_is_unpatched());
    }

    /// The board side of the same fact. `espflash` 4.x never patches the
    /// field, so this is what every real board reports (`m5stack-core#68`).
    #[test]
    fn a_board_hash_of_all_zeros_is_unpatched() {
        let id = Identity { mark: "m".into(), version: "0.1.0".into(), sha256_prefix: "000000000000".into() };
        assert!(id.hash_is_unpatched());
    }

    #[test]
    fn a_real_board_hash_is_not_unpatched() {
        let id = Identity { mark: "m".into(), version: "0.1.0".into(), sha256_prefix: "d862a888b3f7".into() };
        assert!(!id.hash_is_unpatched());
    }

    /// A missing hash is a parse question, not an "unpatched" one.
    #[test]
    fn an_empty_board_hash_is_not_unpatched() {
        let id = Identity { mark: "m".into(), version: "0.1.0".into(), sha256_prefix: String::new() };
        assert!(!id.hash_is_unpatched());
    }

    #[test]
    fn a_patched_hash_is_not_reported_as_unpatched() {
        let d = desc("0.4.3", "display", [0xab; 32]);
        let got = from_elf(&d).expect("found");
        assert!(!got.hash_is_unpatched());
        assert_eq!(got.app_elf_sha256[0], 0xab);
    }
}
