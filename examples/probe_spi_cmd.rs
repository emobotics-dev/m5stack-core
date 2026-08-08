// SPDX-License-Identifier: MIT OR Apache-2.0
//! #18 repro: are SPI `Command::_9Bit`..`_16Bit` transmitted byte-swapped?
//!
//! **On-chip wire probe — no logic analyzer, no jumper wires.** SPI2 runs as
//! master and drives SCLK/MOSI/CS onto three free M-Bus pads; SPI3 runs as a
//! DMA slave whose SCLK/MOSI/CS *inputs* are routed to those same pads through
//! the GPIO matrix. The slave therefore captures the literal bitstream the
//! master puts on the wire — command phase included, which the master's own
//! MISO path can never observe (full-duplex RX only starts at the data phase).
//!
//! Two self-checks come first, so a failure localises itself:
//!   1. **loopback** — the master's MISO is tied to its own MOSI pad, so a
//!      plain full-duplex transfer must read back exactly what it wrote. This
//!      proves the master really clocks and that the pad reads back.
//!   2. **selftest** — a data-only (no command, no address) capture through
//!      the slave. This proves the slave path works before any command bits
//!      are interpreted.
//!
//! Every vector clocks at least 32 bits and the slave always reads exactly 4
//! bytes: ESP32's PDMA rejects RX lengths that are not 4-byte aligned
//! (`InvalidAlignment(Size)`), and a slave asked for more bits than the master
//! clocks would never signal EOF. Each vector's data phase is padded to meet
//! that, which is why the trailing `want` bytes differ between vectors.
//!
//! `want` is derived by hand from the ESP-IDF reference transform
//! (`HAL_SPI_SWAP_DATA_TX(cmd, len) = bswap32(cmd << (32-len))`, `spi_ll.h`),
//! i.e. what the wire *should* carry. `addr16`/`cmd8` are controls that must
//! pass before and after any fix; `cmd9`..`cmd16` are the reported bug;
//! `cmd4`/`cmd1` probe the width<8 case the issue does not cover.
//!
//! **Known limitation — CoreS3 only.** On Fire27 (ESP32) the slave captures
//! nothing: the `loopback` self-check passes (so the master really clocks and
//! the pads read back), but the SPI3 slave never signals EOF and its DMA buffer
//! stays at the 0xEE sentinel. Cause not established — esp-hal marks ESP32 slave
//! support "partial", and its ESP32 `Mode` mapping looks two positions off
//! ESP-IDF (`Mode::_1` programs ck_idle_edge=1/ck_i_edge=1 = IDF's mode 3), but
//! running the slave at `Mode::_3` does not help either. ESP32 PDMA also rejects
//! RX lengths that are not 4-byte aligned, hence `CAP = 4`.
//!
//! Pads (both unconnected on a bare board):
//!   CoreS3  SCLK=GPIO5  MOSI=GPIO6   CS=GPIO7   (M-Bus, free per #15)
//!   Fire27  SCLK=GPIO5  MOSI=GPIO26  CS=GPIO13  (M-Bus, free)
//!
//! Build: `--bin spi_cmd_probe` (no extra features).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

use crate::common::{board, shim};
use embassy_executor::Spawner;
use esp_hal::{
    Blocking,
    dma::DmaRxBuf,
    dma_buffers,
    gpio::AnyPin,
    interrupt::software::SoftwareInterruptControl,
    spi::{
        Mode,
        master::{Address, Command, Config as SpiConfig, DataMode, Spi as SpiMaster},
        slave::{Spi as SpiSlave, dma::SpiDma as SpiSlaveDma},
    },
    time::Rate,
    timer::{AnyTimer, timg::TimerGroup},
};
use m5stack_core::mem::{self, HeapProfile};

m5stack_core::app_desc!();

/// Standard single-bit SPI on the two data lines — the mode every real device
/// uses, and the one the reported bug bites in.
const DM: DataMode = DataMode::SingleTwoDataLines;
/// Bytes the slave captures per vector. Fixed at 4: ESP32 PDMA requires a
/// 4-byte-aligned RX length.
const CAP: usize = 4;
/// Slow enough that the slave samples reliably on both chips.
const SCLK_KHZ: u32 = 1_000;
/// Bound on the `is_done()` spin. A vector is ~32 bits at 1 MHz (~32 us), so
/// anything past this is a stall, not slowness.
const DONE_SPINS: u32 = 20_000_000;

/// One wire-truth measurement.
struct Vector {
    label: &'static str,
    cmd: Command,
    address: Address,
    /// Data phase, padded so the transaction clocks >= `CAP * 8` bits.
    data: &'static [u8],
    /// Expected MOSI bytes, per the ESP-IDF reference transform.
    want: [u8; CAP],
}

/// Let the BSP console's drain task run. `probe` is blocking end to end, so
/// without an await between vectors nothing ever reaches the UART.
async fn drain() {
    embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
}

/// What `probe` did — a stalled slave ends the run rather than hanging the
/// executor (`SpiDmaTransfer::wait`, and its `Drop`, both spin forever).
enum Outcome {
    Ok(SpiSlaveDma<'static, Blocking>, DmaRxBuf),
    Stalled,
}

/// Run one vector: arm the slave, let the master clock the transaction, then
/// compare what landed in the slave's DMA buffer against `want`.
fn probe(
    master: &mut SpiMaster<'static, Blocking>,
    slave: SpiSlaveDma<'static, Blocking>,
    mut rx: DmaRxBuf,
    v: &Vector,
) -> Outcome {
    // Sentinel: anything the DMA did not overwrite stays 0xEE, so a short or
    // absent capture is obvious rather than looking like zeros on the wire.
    rx.as_mut_slice().fill(0xEE);
    rx.set_length(CAP);
    // Kept across the move into `read`: if the transfer never signals done we
    // can still show what the DMA actually wrote, which is the whole point of
    // the run. The buffer is `&'static mut` DRAM, so the pointer stays valid.
    let buf_ptr = rx.as_slice().as_ptr();

    let xfer = match slave.read(CAP, rx) {
        Ok(x) => x,
        Err((e, slave, rx)) => {
            log::error!("{}: slave.read failed: {:?}", v.label, e);
            return Outcome::Ok(slave, rx);
        }
    };

    if let Err(e) = master.half_duplex_write(DM, v.cmd, v.address, 0, v.data) {
        log::error!("{}: master write failed: {:?}", v.label, e);
    }

    let mut spins = 0u32;
    while !xfer.is_done() && spins < DONE_SPINS {
        spins += 1;
    }

    if spins >= DONE_SPINS {
        // SAFETY: `buf_ptr` points at the DMA buffer's first CAP bytes, which
        // are `&'static mut` DRAM kept alive by the leaked transfer below.
        let got = unsafe { core::slice::from_raw_parts(buf_ptr, CAP) };
        log::error!("{:<12} STALLED (no EOF) — dma buffer so far = {:02x?}", v.label, got);
        // Neither `wait()` nor `drop` can be used on a stalled transfer: both
        // spin on `is_done()`. Leak it and end the run.
        core::mem::forget(xfer);
        return Outcome::Stalled;
    }

    let (slave, rx) = xfer.wait();

    let got = &rx.as_slice()[..CAP];
    let verdict = if got == v.want { "PASS" } else { "FAIL" };
    log::info!("{:<12} want={:02x?} got={:02x?}  {}", v.label, v.want, got, verdict);

    Outcome::Ok(slave, rx)
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();

    // Take only what the probe needs, rather than `Board::split` — the probe
    // uses no on-board peripheral (no display, no I2C), just three free pads.
    let tg0 = TimerGroup::new(p.TIMG0);
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    mem::init_heap(HeapProfile::Default);
    esp_rtos::start(AnyTimer::from(tg0.timer0), sw_int.software_interrupt0);
    #[cfg(feature = "fire27")]
    let _console = shim::init_console(
        spawner,
        board::console_serial(p.UART0, AnyPin::from(p.GPIO3), AnyPin::from(p.GPIO1)),
    );
    #[cfg(feature = "cores3")]
    let _console = shim::init_console(spawner, board::console_serial(p.USB_DEVICE));

    log::info!("#18 SPI command-phase wire probe on {} @ {} kHz", board::NAME, SCLK_KHZ);
    drain().await;

    #[cfg(feature = "cores3")]
    let (sclk, mosi, cs) = (AnyPin::from(p.GPIO5), AnyPin::from(p.GPIO6), AnyPin::from(p.GPIO7));
    #[cfg(feature = "fire27")]
    let (sclk, mosi, cs) = (AnyPin::from(p.GPIO5), AnyPin::from(p.GPIO26), AnyPin::from(p.GPIO13));

    // SAFETY: master and slave use each pad in opposite directions (master
    // drives, slave listens) — the classic GPIO-matrix loopback. The extra
    // MOSI clone is the master's own MISO, for the loopback self-check.
    let (sclk_in, mosi_in, cs_in, mosi_back) = unsafe {
        (
            sclk.clone_unchecked(),
            mosi.clone_unchecked(),
            cs.clone_unchecked(),
            mosi.clone_unchecked(),
        )
    };

    // Master first, slave second: the master's output config clears the pad's
    // input enable, so the slave has to claim the input side afterwards.
    // ESP32's SPI slave supports only modes 1 and 3; mode 1 works on both chips.
    let mut master = SpiMaster::new(
        p.SPI2,
        SpiConfig::default().with_frequency(Rate::from_khz(SCLK_KHZ)).with_mode(Mode::_1),
    )
    .expect("SPI2 master init")
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_miso(mosi_back)
    .with_cs(cs);

    // Self-check 1: full-duplex against our own MOSI pad. If this does not
    // read back what it wrote, the master is not clocking (or the pad does not
    // read back) and nothing below is meaningful.
    let mut loopback = [0x5Au8, 0xA5, 0x3C, 0xC3];
    match master.transfer(&mut loopback) {
        Ok(()) => log::info!("loopback     wrote=[5a, a5, 3c, c3] read={:02x?}", loopback),
        Err(e) => log::error!("loopback     transfer failed: {:?}", e),
    }
    drain().await;

    #[cfg(feature = "cores3")]
    let dma_ch = p.DMA_CH0;
    #[cfg(feature = "fire27")]
    let dma_ch = p.DMA_SPI3;

    let mut slave = Some(
        SpiSlave::new(p.SPI3, Mode::_1)
            .with_sck(sclk_in)
            .with_mosi(mosi_in)
            .with_cs(cs_in)
            .with_dma(dma_ch),
    );

    let (rx_buffer, rx_descriptors, _, _) = dma_buffers!(64, 64);
    let mut rx = Some(DmaRxBuf::new(rx_descriptors, rx_buffer).expect("rx dma buf"));

    // Vectors. `want` is the wire the ESP-IDF transform produces; the data
    // phase of each is padded so the transaction clocks >= 32 bits.
    let vectors = [
        // -- self-check 2: no command, no address, data only ----------------
        Vector {
            label: "selftest",
            cmd: Command::None,
            address: Address::None,
            data: &[0x5A, 0xA5, 0x3C, 0xC3],
            want: [0x5A, 0xA5, 0x3C, 0xC3],
        },
        // -- controls: must pass before and after any fix -------------------
        // The address path already left-aligns (`value << (32 - width)`), so a
        // 16-bit *address* is the in-run proof that 16-bit big-endian is what
        // the wire should carry.
        Vector {
            label: "addr16 DA00",
            cmd: Command::None,
            address: Address::_16Bit(0xDA00, DM),
            data: &[0x5A, 0xA5],
            want: [0xDA, 0x00, 0x5A, 0xA5],
        },
        Vector {
            label: "cmd8 9A",
            cmd: Command::_8Bit(0x9A, DM),
            address: Address::None,
            data: &[0x5A, 0xA5, 0x3C],
            want: [0x9A, 0x5A, 0xA5, 0x3C],
        },
        // -- the reported bug: widths 9..16 ---------------------------------
        Vector {
            label: "cmd16 DA00",
            cmd: Command::_16Bit(0xDA00, DM),
            address: Address::None,
            data: &[0x5A, 0xA5],
            want: [0xDA, 0x00, 0x5A, 0xA5],
        },
        Vector {
            label: "cmd16 1234",
            cmd: Command::_16Bit(0x1234, DM),
            address: Address::None,
            data: &[0x5A, 0xA5],
            want: [0x12, 0x34, 0x5A, 0xA5],
        },
        // 9 bits (1 1010 1010) then 5A A5 3C -> 33 bits.
        Vector {
            label: "cmd9 1AA",
            cmd: Command::_9Bit(0x1AA, DM),
            address: Address::None,
            data: &[0x5A, 0xA5, 0x3C],
            want: [0xD5, 0x2D, 0x52, 0x9E],
        },
        // 12 bits (1010 1011 1100) then 5A A5 3C -> 36 bits.
        Vector {
            label: "cmd12 ABC",
            cmd: Command::_12Bit(0xABC, DM),
            address: Address::None,
            data: &[0x5A, 0xA5, 0x3C],
            want: [0xAB, 0xC5, 0xAA, 0x53],
        },
        // -- widths < 8: NOT covered by the issue; left-aligned per ESP-IDF --
        // 4 bits (1010) then 5A A5 3C C3 -> 36 bits.
        Vector {
            label: "cmd4 A",
            cmd: Command::_4Bit(0xA, DM),
            address: Address::None,
            data: &[0x5A, 0xA5, 0x3C, 0xC3],
            want: [0xA5, 0xAA, 0x53, 0xCC],
        },
        // 1 bit (1) then 5A A5 3C C3 -> 33 bits.
        Vector {
            label: "cmd1 1",
            cmd: Command::_1Bit(0x1, DM),
            address: Address::None,
            data: &[0x5A, 0xA5, 0x3C, 0xC3],
            want: [0xAD, 0x52, 0x9E, 0x61],
        },
    ];

    // Sweep repeatedly: on CoreS3 the USB-Serial-JTAG CDC drops across a
    // probe-rs reset, so a one-shot report is easy to miss. Any capture window
    // now catches a whole sweep.
    let mut round = 0u32;
    'sweep: loop {
        round += 1;
        drain().await;
        log::info!("--- sweep {} ---", round);
        for v in &vectors {
            // The BSP console drains from its own task, so a run of blocking
            // probe calls would buffer every line and show nothing until the
            // end — or nothing at all if one stalls. Yield after each vector.
            drain().await;
            match probe(&mut master, slave.take().unwrap(), rx.take().unwrap(), v) {
                Outcome::Ok(s, r) => {
                    slave = Some(s);
                    rx = Some(r);
                }
                Outcome::Stalled => {
                    drain().await;
                    log::error!("#18 probe aborted after {}", v.label);
                    break 'sweep;
                }
            }
        }
        drain().await;
        log::info!("#18 sweep {} done", round);
        embassy_time::Timer::after(embassy_time::Duration::from_secs(3)).await;
    }

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(5)).await;
    }
}
