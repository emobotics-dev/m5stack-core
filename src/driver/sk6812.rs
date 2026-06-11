// SPDX-License-Identifier: MIT OR Apache-2.0
//! SK6812 addressable-RGB LED-strip driver (M5GO Battery Bottom light bars).
//!
//! The M5GO/FIRE Battery Bottom carries **10 SK6812 3535 RGB LEDs** (two bars of
//! five) on a single one-wire data line. SK6812 uses the same NRZ bit encoding
//! as WS2812 — 24 bits per LED in **G,R,B** order, MSB first — so this is driven
//! by the RMT peripheral, one [`PulseCode`] per bit, like the 1-Wire driver.
//!
//! ## Pin (this is the subtle part across boards)
//!
//! The bottom's LED data line is wired to a fixed *physical* M-Bus pin (pin 23,
//! the classic `I2S_OUT` position). That physical pin maps to a different GPIO
//! per core, so pass the right one:
//!   - **Fire27 (ESP32):**   `GPIO15`  (M-Bus pin 23)
//!   - **CoreS3 (ESP32-S3):** `GPIO13`  (M-Bus pin 23)
//!
//! On **CoreS3** the bars are also unpowered until the M-Bus 5 V rail is enabled
//! via the AW9523 — call [`crate::driver::aw9523b::Aw9523bDriver::enable_bus_5v`]
//! first (see its guard notes). On the Fire that rail is always present.
//!
//! ## Timing
//!
//! RMT source clock 80 MHz, `clk_divider = 1` → 12.5 ns/tick. SK6812 nominal:
//! T0H 0.3 µs / T0L 0.9 µs, T1H 0.6 µs / T1L 0.6 µs, reset ≥ 80 µs low.
//!
//! Refs: SK6812 datasheet; <https://docs.m5stack.com/en/base/m5go_bottom>
use esp_hal::{
    Async,
    gpio::{AnyPin, Level},
    peripherals::RMT,
    rmt::{Channel, PulseCode, Rmt, Tx, TxChannelConfig, TxChannelCreator},
    time::Rate,
};
use thiserror_no_std::Error;

/// Max LEDs this driver will drive in one frame (M5GO bottom uses 10).
/// Sets the size of the per-frame pulse buffer; bump if you chain more.
pub const MAX_LEDS: usize = 16;
/// 24 bits/LED + trailing reset code + end marker.
const MAX_CODES: usize = MAX_LEDS * 24 + 2;

// Bit timing in 12.5 ns ticks (80 MHz, divider 1).
const T0H: u16 = 24; // 0.30 µs
const T0L: u16 = 72; // 0.90 µs
const T1H: u16 = 48; // 0.60 µs
const T1L: u16 = 48; // 0.60 µs
const RESET_HALF: u16 = 4000; // 2 × 50 µs = 100 µs low latch

/// A single LED colour (8 bits per channel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const OFF: Rgb = Rgb { r: 0, g: 0, b: 0 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

#[derive(Debug, Error)]
pub enum Sk6812Error {
    #[error("Failed to init RMT")]
    Rmt(#[from] esp_hal::rmt::Error),
    #[error("RMT config error")]
    Config,
    #[error("Too many LEDs (max {MAX_LEDS})")]
    TooManyLeds,
}

pub struct Sk6812Driver {
    channel: Channel<'static, Async, Tx>,
}

impl Sk6812Driver {
    /// Claim the RMT peripheral (TX channel 0) and drive `pin` as the LED data
    /// line. Note: the RMT peripheral is taken whole here — it cannot also be
    /// handed to the 1-Wire temperature driver in the same app.
    pub fn new(rmt: RMT<'static>, pin: AnyPin<'static>) -> Result<Self, Sk6812Error> {
        let rmt = Rmt::new(rmt, Rate::from_mhz(80))
            .map_err(|_| Sk6812Error::Config)?
            .into_async();
        let channel = rmt
            .channel0
            .configure_tx(
                &TxChannelConfig::default()
                    .with_clk_divider(1)
                    .with_idle_output(true)
                    .with_idle_output_level(Level::Low),
            )
            .map_err(|_| Sk6812Error::Config)?
            .with_pin(pin);
        Ok(Self { channel })
    }

    /// Push a frame of colours to the strip (LED 0 first). Holds the line low
    /// afterwards to latch. Errors if `colors.len() > MAX_LEDS`.
    pub async fn write(&mut self, colors: &[Rgb]) -> Result<(), Sk6812Error> {
        if colors.len() > MAX_LEDS {
            return Err(Sk6812Error::TooManyLeds);
        }
        let one = PulseCode::new(Level::High, T1H, Level::Low, T1L);
        let zero = PulseCode::new(Level::High, T0H, Level::Low, T0L);

        let mut buf: heapless::Vec<PulseCode, MAX_CODES> = heapless::Vec::new();
        for c in colors {
            // SK6812 wire order is G, R, B, MSB first.
            for byte in [c.g, c.r, c.b] {
                for bit in 0..8 {
                    let code = if byte & (0x80 >> bit) != 0 { one } else { zero };
                    // Buffer holds MAX_LEDS*24 + 2 and we checked the LED count.
                    let _ = buf.push(code);
                }
            }
        }
        // Trailing low pulse → reset/latch (≥ 80 µs), then the required end
        // marker (RMT errors with `EndMarkerMissing` without a final zero-length
        // code; the marker idles the line low, holding the latch).
        let _ = buf.push(PulseCode::new(
            Level::Low,
            RESET_HALF,
            Level::Low,
            RESET_HALF,
        ));
        let _ = buf.push(PulseCode::end_marker());

        self.channel.transmit(&buf).await?;
        Ok(())
    }

    /// Convenience: set every LED in a strip of `len` to the same colour.
    pub async fn fill(&mut self, color: Rgb, len: usize) -> Result<(), Sk6812Error> {
        let len = len.min(MAX_LEDS);
        let mut frame: heapless::Vec<Rgb, MAX_LEDS> = heapless::Vec::new();
        for _ in 0..len {
            let _ = frame.push(color);
        }
        self.write(&frame).await
    }
}
