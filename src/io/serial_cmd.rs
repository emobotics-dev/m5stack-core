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
        Async,
        gpio::AnyPin,
        peripherals::UART0,
        uart::{Config, UartRx},
    };

    pub struct SerialCmdResources<'d> {
        pub uart: UART0<'d>,
        pub rx_pin: AnyPin<'d>,
    }

    pub type SerialRx<'d> = UartRx<'d, Async>;

    pub fn take_rx(r: SerialCmdResources<'static>) -> SerialRx<'static> {
        UartRx::new(r.uart, Config::default())
            .expect("UART0 RX init")
            .with_rx(r.rx_pin)
            .into_async()
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
