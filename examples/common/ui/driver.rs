// SPDX-License-Identifier: MIT OR Apache-2.0
//! oxivgl flush glue over the BSP's DMA display ([`board::spi2::DisplayBus`]).

use esp_hal::ram;
use m5stack_core::board::spi2::{DisplayBus, DisplayOnlyType};
use oxivgl::flush_pipeline::{DisplayOutput, UiError, flush_frame_buffer};

/// Owns the BSP display (and, on Fire27, keeps the backlight pin alive). The
/// single [`DisplayOutput`] method is what LVGL's flush task calls with each
/// dirty rectangle.
pub struct DisplayDriver {
    display: DisplayOnlyType,
    #[cfg(feature = "fire27")]
    _backlight: esp_hal::gpio::Output<'static>,
}

// SAFETY: `DisplayDriver` holds `Spi<Async>`, whose `PhantomData<*const ()>`
// makes it `!Send` to guard against accidental cross-thread sharing. On the
// single-core ESP32/ESP32-S3 the `flush_task` is the sole owner; no concurrent
// access occurs, so moving it onto the interrupt executor is sound.
unsafe impl Send for DisplayDriver {}

impl DisplayDriver {
    pub fn new(bus: DisplayBus) -> Self {
        #[cfg(feature = "fire27")]
        {
            Self { display: bus.display, _backlight: bus.backlight }
        }
        #[cfg(feature = "cores3")]
        {
            Self { display: bus.display }
        }
    }
}

impl DisplayOutput for DisplayDriver {
    async fn show_raw_data(
        &mut self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        data: &[u8],
    ) -> Result<(), UiError> {
        self.display
            .show_raw_data(x, y, w, h, data)
            .await
            .map_err(|_| UiError::Display)
    }
}

/// Flush task for the **shared-executor** pipeline (`examples/lvgl/`): drains
/// oxivgl's draw channel and pushes pixels to the panel from an
/// `InterruptExecutor`. Placed in RAM so it never stalls on flash access.
///
/// A new UI should run the flush on its own thread instead — see
/// `docs/lvgl-render-pipeline.md`; that path calls `flush_frame_buffer`
/// directly and does not need this task.
#[embassy_executor::task]
#[ram]
pub async fn flush_task(driver: DisplayDriver) -> ! {
    flush_frame_buffer(driver).await
}
