// SPDX-License-Identifier: MIT OR Apache-2.0
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use esp_hal::{Async, i2c::master::I2c};

/// Shared I2C bus for cooperative tasks on one core.
///
/// NOT `Send`/`Sync`, deliberately. `I2c<'static, Async>` is `!Send` in esp-hal
/// (`PhantomData<*const ()>`) because an async peripheral's wakers and interrupt
/// state belong to the core that armed it with `into_async()`.
///
/// This type used to carry `unsafe impl Send`/`Sync` justified by two claims:
/// that AW9523B init runs on PRO before APP starts, and that the PPS/AXP2101
/// tasks are cooperative. The first stopped being true once consumers moved I2C
/// bring-up onto the APP core (so the IRQ binds there instead of beside the BLE
/// controller's level-1 IRQ on PRO). The second is an argument about *access*,
/// while `Send` is a claim about *moving the value between threads* — a
/// different property that cooperative scheduling does not establish.
///
/// Neither claim can be supported, so the impls are gone. `!Send` is now the
/// compiler's to enforce: a consumer that tries to move the bus to another core
/// gets a build error instead of silent, unchecked breakage.
///
/// `CriticalSectionRawMutex` stays for the flag check, which must be safe
/// against preemption from any context; the CS is not held across the I2C
/// transaction (the guard is released at the next `.await`).
pub struct SharedI2cBus(Mutex<CriticalSectionRawMutex, I2c<'static, Async>>);

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
