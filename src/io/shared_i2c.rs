// SPDX-License-Identifier: BSD-3-Clause
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use esp_hal::{Async, i2c::master::I2c};

/// Shared I2C bus for cooperative tasks on APP core and init on PRO core.
///
/// # Safety
/// `I2c<'static, Async>` is `!Send` due to `PhantomData<*const ()>`. The `unsafe impl
/// Send/Sync` covers only that peripheral-type constraint; access is safe because:
/// - AW9523B init runs on PRO core before APP core starts (no concurrent access)
/// - PPS/AXP2101 tasks run cooperatively on APP core (never truly concurrent)
/// `CriticalSectionRawMutex` is used for correctness: the mutex flag check is guarded
/// against preemption from any context, at the cost of a brief global spinlock per
/// lock/unlock. Safe because the CS is held only for the flag check, not across the
/// I2C transaction (the lock guard is released at the next `.await`).
pub struct SharedI2cBus(Mutex<CriticalSectionRawMutex, I2c<'static, Async>>);
// Safety: see above.
unsafe impl Send for SharedI2cBus {}
unsafe impl Sync for SharedI2cBus {}

impl SharedI2cBus {
    pub const fn new(i2c: I2c<'static, Async>) -> Self {
        Self(Mutex::new(i2c))
    }

    pub fn lock(
        &self,
    ) -> impl core::future::Future<
        Output = embassy_sync::mutex::MutexGuard<'_, CriticalSectionRawMutex, I2c<'static, Async>>,
    > {
        self.0.lock()
    }
}
