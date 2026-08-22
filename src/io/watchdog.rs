// SPDX-License-Identifier: MIT OR Apache-2.0
//! Production hardware watchdog (RWDT) backstop.

use embassy_time::{Duration, Timer};
use esp_hal::rtc_cntl::{Rtc, RwdtStage};

/// Arm the RWDT (Stage0 = hardware system reset on timeout) and feed it
/// forever from the calling executor.
///
/// Spawn this on the executor whose wedging you want to catch: if that
/// executor stops being scheduled, the feed stops and the RWDT hard-resets
/// the chip `timeout_secs` later. NOT a substitute for bounding individual
/// operations — a subsystem hang with the executor still alive keeps feeding.
///
/// `Rtc` is consumed; construct it from
/// [`SystemResources::lpwr`](crate::board::SystemResources::lpwr).
pub async fn watchdog_feed_loop(
    mut rtc: Rtc<'static>,
    timeout_secs: u64,
    feed_every_secs: u64,
) -> ! {
    rtc.rwdt.set_timeout(
        RwdtStage::Stage0,
        esp_hal::time::Duration::from_secs(timeout_secs),
    );
    rtc.rwdt.enable();
    info!(
        "[wdt] RWDT armed ({} s reset), feeding every {} s",
        timeout_secs, feed_every_secs
    );
    loop {
        rtc.rwdt.feed();
        Timer::after(Duration::from_secs(feed_every_secs)).await;
    }
}
