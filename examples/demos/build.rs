// SPDX-License-Identifier: MIT OR Apache-2.0
fn main() {
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    #[cfg(feature = "identity")]
    m5stack_core_build::emit_identity_env("");
}
