// SPDX-License-Identifier: MIT OR Apache-2.0
//! Build-time guard: PSRAM needs an optimizing build.
//!
//! Cargo injects both vars below into every build script automatically —
//! `OPT_LEVEL` from the active profile, `CARGO_FEATURE_PSRAM` iff a downstream
//! crate turned on the `psram` feature. Nothing here has to be set by hand.
//!
//! We observe *this crate's* opt-level. With a normal unified profile that is
//! the same level the application is built at, so the check is exact for the
//! common footgun (`cargo build` with no optimization). It could only diverge
//! if someone deliberately pins a per-package profile override for this crate.

fn main() {
    // Applies to example targets only, so a consumer linking this crate is
    // unaffected. The examples are firmware images and need esp-hal's script.
    println!("cargo:rustc-link-arg-examples=-Tlinkall.x");
    let psram = std::env::var_os("CARGO_FEATURE_PSRAM").is_some();
    let opt0 = std::env::var("OPT_LEVEL").as_deref() == Ok("0");
    if psram && opt0 {
        println!(
            "cargo::error=m5stack-core `psram` feature requires opt-level > 0: PSRAM SPI \
             timing calibration is unreliable at opt-level 0. Set opt-level in your [profile.*]."
        );
    }
}
