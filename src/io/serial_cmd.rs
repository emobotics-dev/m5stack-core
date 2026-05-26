// SPDX-License-Identifier: MIT OR Apache-2.0
//! HIL serial-cmd transport — exposes a `Read` endpoint for the
//! `alternator-regulator::serial_cmd` parser.
//!
//! - fire27 (ESP32): UART0 RX (GPIO3), already routed through the on-board
//!   USB-serial bridge used by `espflash`. TX side is left to `esp-println`.
//! - cores3 (ESP32-S3): native USB-Serial-JTAG CDC. Independent USB
//!   endpoint from probe-rs JTAG/RTT.

#[cfg(feature = "fire27")]
mod imp {
    use esp_hal::{
        Blocking,
        gpio::AnyPin,
        peripherals::UART0,
        uart::{Config, UartRx},
    };

    pub struct SerialCmdResources<'d> {
        pub uart: UART0<'d>,
        pub rx_pin: AnyPin<'d>,
    }

    /// Blocking UartRx — caller drives reads via polling + Timer.
    ///
    /// `into_async()` is *not* called here. Two reasons:
    /// 1. The blocking variant works fine with the polled `run_polled` loop
    ///    and avoids registering a UART interrupt handler entirely.
    /// 2. **Init ordering matters:** `UartRx::new(...)` (even without
    ///    `into_async`) must run *after* the BLE radio has finished initializing
    ///    on ESP32 — calling it earlier hangs BLE setup silently (LCD goes
    ///    white, no `Device Address` log). The caller is responsible for
    ///    deferring `take_rx` until radio is up.
    pub type SerialRx<'d> = UartRx<'d, Blocking>;

    pub fn take_rx(r: SerialCmdResources<'static>) -> SerialRx<'static> {
        UartRx::new(r.uart, Config::default())
            .expect("UART0 RX init")
            .with_rx(r.rx_pin)
    }
}

#[cfg(feature = "cores3")]
mod imp {
    use esp_hal::{
        Async,
        peripherals::USB_DEVICE,
        usb_serial_jtag::UsbSerialJtag,
    };

    pub struct SerialCmdResources<'d> {
        pub usb: USB_DEVICE<'d>,
    }

    pub type SerialRx<'d> = UsbSerialJtag<'d, Async>;

    pub fn take_rx(r: SerialCmdResources<'static>) -> SerialRx<'static> {
        UsbSerialJtag::new(r.usb).into_async()
    }
}

pub use imp::{SerialRx, SerialCmdResources, take_rx};
