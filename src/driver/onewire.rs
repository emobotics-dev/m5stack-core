// SPDX-License-Identifier: MIT OR Apache-2.0
//! 1-Wire master over the ESP32 RMT peripheral (async).
//!
//! Vendored from the `esp-hal-rmt-onewire` crate (v0.4.0) by **jonored**
//! — <https://github.com/jonored/esp-hal-rmt-onewire> — dual-licensed
//! MIT OR Apache-2.0 (its `LICENSE-MIT` reads "Copyright 2021 esp-rs").
//! Original authorship and copyright are retained, with thanks to the author.
//!
//! Adapted for in-tree use: updated for the esp-hal 1.1 RMT API, and the
//! const-generic `exchange_bits` reworked to a slice form (dropping the
//! `generic_const_exprs` nightly feature) — modified from the original.

use core::fmt::LowerHex;
use embassy_futures::select::*;
use esp_hal::rmt::{Rx, Tx};
use esp_hal::{
    Async,
    gpio::{
        DriveMode, DriveStrength, Flex, InputConfig, Level, OutputConfig, Pin, Pull,
        interconnect::*,
    },
    rmt::{
        Channel, ConfigError, PulseCode, RxChannelConfig, RxChannelCreator, TxChannelConfig,
        TxChannelCreator,
    },
};

/// Async 1-Wire bus master built on a pair of RMT TX/RX channels.
pub struct OneWire<'a> {
    rx: Channel<'a, Async, Rx>,
    tx: Channel<'a, Async, Tx>,
    input: InputSignal<'a>,
}

impl<'a> OneWire<'a> {
    /// Create a 1-Wire master driving `pin` (open-drain, pull-up) using the
    /// supplied RMT TX and RX channel creators.
    pub fn new<Txc: TxChannelCreator<'a, Async>, Rxc: RxChannelCreator<'a, Async>, P: Pin + 'a>(
        txcc: Txc,
        rxcc: Rxc,
        pin: P,
    ) -> Result<Self, Error> {
        let rx_config = RxChannelConfig::default()
            .with_clk_divider(80)
            .with_idle_threshold(1000)
            .with_filter_threshold(10)
            .with_carrier_modulation(false);
        let tx_config = TxChannelConfig::default()
            .with_clk_divider(80)
            .with_carrier_modulation(false);

        let mut pin: Flex = Flex::new(pin);

        pin.apply_input_config(&InputConfig::default().with_pull(Pull::Up));
        pin.apply_output_config(
            &OutputConfig::default()
                .with_drive_mode(DriveMode::OpenDrain)
                .with_drive_strength(DriveStrength::_40mA),
        );
        pin.set_input_enable(true);
        pin.set_output_enable(true);
        let (input, output) = pin.split();

        let tx = txcc
            .configure_tx(&tx_config)
            .map_err(Error::ConfigError)?
            .with_pin(output.with_output_inverter(true));
        let rx = rxcc
            .configure_rx(&rx_config)
            .map_err(Error::ConfigError)?
            .with_pin(input.clone().with_input_inverter(true));

        Ok(OneWire { rx, tx, input })
    }
}

impl<'a> OneWire<'a> {
    /// Issue a 1-Wire reset pulse and return `true` if at least one device
    /// responded with a presence pulse.
    pub async fn reset(&mut self) -> Result<bool, Error> {
        let data = [
            PulseCode::new(Level::Low, 60, Level::High, 600),
            PulseCode::new(Level::Low, 600, Level::Low, 0),
            PulseCode::end_marker(),
        ];
        let mut indata = [PulseCode::end_marker(); 10];

        let _res = self.send_and_receive(&mut indata, &data).await?;

        Ok(indata[0].length1() > 0
            && indata[0].length2() > 0
            && indata[1].length1() > 100
            && indata[1].length1() < 200)
    }

    /// Transmit `data` while simultaneously sampling the bus into `indata`,
    /// returning the number of received RMT symbols.
    pub async fn send_and_receive(
        &mut self,
        indata: &mut [PulseCode],
        data: &[PulseCode],
    ) -> Result<usize, Error> {
        let delay = [PulseCode::new(Level::Low, 10000, Level::Low, 0)]; // timeout delay for 30ms using the RMT tx peripheral.
        if self.input.level() == Level::Low {
            Err(Error::InputNotHigh)?;
        }
        // This relies on select polling in order to set up the rx & tx registers, which is not strictly documented behavior.
        let res = select(self.rx.receive(indata), async {
            let r = self.tx.transmit(data).await;
            let _ = self.tx.transmit(&delay).await;
            r
        })
        .await;

        // Internal interface to cancel the TX-based timeout seems not accessible on c3.
        // Example is running perfectly fine with slightly reduced timeout avlue.
        // self.tx.stop_tx();

        match res {
            Either::First(Ok(r)) => Ok(r),
            Either::First(Err(r)) => Err(Error::ReceiveError(r)),
            Either::Second(Ok(())) => Err(Error::ReceiveTimedOut),
            Either::Second(Err(e)) => Err(Error::SendError(e)),
        }
    }

    const ZERO_BIT_LEN: u16 = 70;
    const ONE_BIT_LEN: u16 = 3;

    /// Encode a single 1-Wire bit as an RMT pulse code (write/read time slot).
    pub fn encode_bit(bit: bool) -> PulseCode {
        if bit {
            PulseCode::new(
                Level::High,
                Self::ONE_BIT_LEN,
                Level::Low,
                Self::ZERO_BIT_LEN,
            )
        } else {
            PulseCode::new(
                Level::High,
                Self::ZERO_BIT_LEN,
                Level::Low,
                Self::ONE_BIT_LEN,
            )
        }
    }

    /// Decode a sampled RMT pulse code back into the 1-Wire bit value.
    pub fn decode_bit(code: PulseCode) -> bool {
        let len = code.length1();
        len < 20
    }

    /// Write one byte (LSB first) and read the byte the bus returns in the
    /// same time slots.
    pub async fn exchange_byte(&mut self, byte: u8) -> Result<u8, Error> {
        let mut data = [PulseCode::end_marker(); 10];
        let mut indata = [PulseCode::end_marker(); 10];
        for n in 0..8 {
            data[n] = Self::encode_bit(0 != byte & 1 << n);
        }
        let _res = self.send_and_receive(&mut indata, &data).await?;
        let mut res: u8 = 0;
        for n in 0..8 {
            if Self::decode_bit(indata[n]) {
                res |= 1 << n;
            }
        }
        Ok(res)
    }

    /// Write one byte (LSB first) without reading the response.
    pub async fn send_byte(&mut self, byte: u8) -> Result<(), Error> {
        let mut data = [PulseCode::end_marker(); 10];
        for n in 0..8 {
            data[n] = Self::encode_bit(0 != byte & 1 << n);
        }
        let _res = self.tx.transmit(&data).await?;
        Ok(())
    }

    /// Maximum number of bits a single [`OneWire::exchange_bits`] call accepts.
    const MAX_EXCHANGE_BITS: usize = 8;

    /// Write the bits in `bits` and read the bus response into `out` in the
    /// same time slots. `bits` and `out` must have equal length, and at most
    /// [`Self::MAX_EXCHANGE_BITS`] elements.
    ///
    /// This replaces the original const-generic `exchange_bits<const N>` (which
    /// required `generic_const_exprs`) with a slice-based API backed by a small
    /// fixed-capacity buffer. The RMT framing is identical: each bit maps to one
    /// `PulseCode` followed by a trailing `end_marker()`.
    pub async fn exchange_bits(&mut self, bits: &[bool], out: &mut [bool]) -> Result<(), Error> {
        debug_assert_eq!(bits.len(), out.len());
        debug_assert!(bits.len() <= Self::MAX_EXCHANGE_BITS);
        let n_bits = bits.len();

        // One PulseCode per bit, plus a trailing end_marker (pre-filled).
        let mut data = [PulseCode::end_marker(); Self::MAX_EXCHANGE_BITS + 1];
        let mut indata = [PulseCode::end_marker(); Self::MAX_EXCHANGE_BITS + 1];
        for n in 0..n_bits {
            data[n] = Self::encode_bit(bits[n]);
        }
        let _res = self
            .send_and_receive(&mut indata[..n_bits + 1], &data[..n_bits + 1])
            .await?;
        for n in 0..n_bits {
            out[n] = Self::decode_bit(indata[n]);
        }
        Ok(())
    }

    /// Write a 64-bit value (typically a ROM address) least-significant byte
    /// first.
    pub async fn send_u64(&mut self, val: u64) -> Result<(), Error> {
        for byte in val.to_le_bytes() {
            self.send_byte(byte).await?;
        }
        Ok(())
    }

    /// Write a 64-bit 1-Wire ROM [`Address`].
    pub async fn send_address(&mut self, val: Address) -> Result<(), Error> {
        self.send_u64(val.0).await
    }
}

/// Errors returned by 1-Wire bus operations.
#[derive(Debug)]
pub enum Error {
    /// The bus was not idle-high before a transaction (missing pull-up?).
    InputNotHigh,
    /// No presence/response was sampled before the RMT timeout elapsed.
    ReceiveTimedOut,
    /// The RMT RX channel reported an error.
    ReceiveError(esp_hal::rmt::Error),
    /// The RMT TX channel reported an error.
    SendError(esp_hal::rmt::Error),
    /// An RMT channel could not be configured.
    ConfigError(ConfigError),
}

impl From<esp_hal::rmt::Error> for Error {
    fn from(e: esp_hal::rmt::Error) -> Error {
        Error::SendError(e)
    }
}

/// A 64-bit 1-Wire ROM address (family code, serial, CRC).
#[derive(PartialEq, Eq, Clone, Copy, Hash)]
pub struct Address(pub u64);

impl LowerHex for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl core::fmt::Debug for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> Result<(), core::fmt::Error> {
        core::write!(f, "{:X?}", self.0.to_le_bytes())
    }
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> Result<(), core::fmt::Error> {
        for k in self.0.to_le_bytes() {
            core::write!(f, "{:X}", k)?;
        }
        Ok(())
    }
}

/// State machine for the 1-Wire ROM Search algorithm, enumerating every device
/// address on the bus across repeated [`Search::next`] calls.
pub struct Search {
    command: u8,
    address: u64,
    #[cfg(feature = "search-masks")]
    address_mask: u64,
    last_discrepancy: Option<usize>,
    complete: bool,
}

/// Errors returned while iterating a ROM [`Search`].
#[derive(Debug)]
pub enum SearchError {
    /// All devices have already been enumerated.
    SearchComplete,
    /// No device responded to the search.
    NoDevicesPresent,
    /// An underlying bus error occurred.
    BusError(Error),
}

impl From<Error> for SearchError {
    fn from(e: Error) -> SearchError {
        SearchError::BusError(e)
    }
}

impl Search {
    /// Start a normal (0xF0) ROM search over all devices on the bus.
    pub fn new() -> Search {
        Search {
            command: 0xF0,
            address: 0,
            #[cfg(feature = "search-masks")]
            address_mask: 0,
            last_discrepancy: None,
            complete: false,
        }
    }

    /// Start an alarm (0xEC) search, enumerating only devices in an alarm state.
    pub fn new_alarm() -> Search {
        Search {
            command: 0xEC,
            address: 0,
            #[cfg(feature = "search-masks")]
            address_mask: 0,
            last_discrepancy: None,
            complete: false,
        }
    }

    /// Start a search constrained to addresses matching `fixed_bits` under
    /// `bit_mask`.
    #[cfg(feature = "search-masks")]
    pub fn new_with_mask(fixed_bits: u64, bit_mask: u64) -> Search {
        Search {
            command: 0xEC,
            address: fixed_bits,
            address_mask: bit_mask,
            last_discrepancy: None,
            complete: false,
        }
    }

    /// Advance the search and return the next device [`Address`], or a
    /// [`SearchError`] once enumeration finishes or the bus errors.
    pub async fn next<'d>(&mut self, ow: &mut OneWire<'d>) -> Result<Address, SearchError> {
        if self.complete {
            return Err(SearchError::SearchComplete);
        }
        let have_devices = ow.reset().await?;
        let mut last_zero = None;
        ow.send_byte(self.command).await?;
        if have_devices {
            for id_bit_number in 0..64 {
                let mut id_bits = [false; 2];
                ow.exchange_bits(&[true, true], &mut id_bits).await?;
                let search_direction = match id_bits {
                    #[cfg(feature = "search-masks")]
                    _ if self.address_mask & (1 << id_bit_number) != 0 => {
                        self.address & (1 << id_bit_number) != 0
                    }
                    [false, true] => false,
                    [true, false] => true,
                    [true, true] => {
                        return Err(SearchError::NoDevicesPresent);
                    }
                    [false, false] => {
                        if self.last_discrepancy == Some(id_bit_number) {
                            true
                        } else if Some(id_bit_number) > self.last_discrepancy {
                            last_zero = Some(id_bit_number);
                            false
                        } else {
                            self.address & (1 << id_bit_number) != 0
                        }
                    }
                };
                if search_direction {
                    self.address |= 1 << id_bit_number;
                } else {
                    self.address &= !(1 << id_bit_number);
                }
                let mut sent = [false; 1];
                ow.exchange_bits(&[search_direction], &mut sent).await?;
            }
            self.last_discrepancy = last_zero;
            self.complete = last_zero.is_none();
            Ok(Address(self.address))
        } else {
            Err(SearchError::NoDevicesPresent)
        }
    }
}

impl Default for Search {
    fn default() -> Self {
        Self::new()
    }
}
