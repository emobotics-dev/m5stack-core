// SPDX-License-Identifier: MIT OR Apache-2.0
//! #16 (ESP32-S3): is `GDMA_IN_SUC_EOF` already set when `SPI_TRANS_DONE`
//! fires for a DMA read?
//!
//! **On-chip, no external hardware.** SPI2 runs as master with a GDMA RX
//! channel; MOSI is looped back to the master's own MISO pad (same trick as
//! `spi_cmd_probe`) purely to keep the bus driven — the received data itself
//! is irrelevant, only DMA completion *timing* is measured.
//!
//! A real interrupt handler is bound to SPI2's `SPI_TRANS_DONE` (the only
//! interrupt source enabled). Inside it, the handler reads the GDMA RX
//! channel's raw `IN_SUC_EOF` bit *before touching anything else* and tallies
//! whether it was already set. ESP-IDF's own driver chains DMA reads purely
//! on `trans_done` with no EOF wait (`spi_master.c` `spi_intr()`), which is
//! only correct if `in_suc_eof` is always at-or-before `trans_done` from an
//! ISR's point of view — that is the analytic claim `esp_hal::spi::master`
//! and any other DMA SPI driver on this chip relies on but has never been
//! measured. `spi_ll.h` says the causal order is `trans_done` -> inlink EOF
//! -> descriptor write-back -> `in_suc_eof`, so a non-zero "not yet set"
//! count would mean a driver that re-arms from the ISR can race the
//! descriptor write-back and observe a stale tail.
//!
//! Sweeps length (4 B / 64 B / 3169 B, deliberately unaligned since GDMA has
//! no 4-byte RX restriction on this chip, unlike ESP32 PDMA) and SPI clock
//! (1 / 8 / 40 MHz) — 9 combos x 10,000 transfers each, ~90,000 total,
//! matching the issue's "~10^5" ask. Total runtime is dominated by the
//! 3169 B / 1 MHz combo alone (~25 ms/transfer x 10,000 ~= 4 min); the whole
//! sweep is ~5-6 minutes.
//!
//! Pads (M-Bus, free per #15, unconnected on a bare board):
//!   CoreS3  SCLK=GPIO5  MOSI=GPIO6 (looped to MISO)  CS=GPIO7
//!
//! Build: `--bin gdma_eof_probe --no-default-features --features cores3`
//! (ESP32-S3 only — this chip's GDMA `IN_SUC_EOF` accessor path
//! (`DMA::regs().ch(n).in_int().raw()`) is esp32s3-specific).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use core::sync::atomic::{AtomicU32, Ordering};

use demos::{board, shim};
use embassy_executor::Spawner;
use esp_hal::{
    dma::DmaRxBuf,
    dma_buffers,
    gpio::AnyPin,
    handler,
    interrupt::{Priority, software::SoftwareInterruptControl},
    peripherals::{DMA, SPI2},
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi as SpiMaster, SpiInterrupt},
    },
    time::Rate,
    timer::{AnyTimer, timg::TimerGroup},
};
use m5stack_core::mem::{self, HeapProfile};

m5stack_core::app_desc!();

/// `p.DMA_CH0` — the only GDMA channel this probe uses, and the index the
/// ISR reads back on the raw register path.
const DMA_CHANNEL: usize = 0;

/// Transfer lengths to sweep. 3169 is deliberately unaligned: GDMA (unlike
/// ESP32 PDMA) has no 4-byte RX length restriction on this chip.
const LENGTHS: [usize; 3] = [4, 64, 3169];
/// SPI clocks to sweep, MHz.
const CLOCKS_MHZ: [u32; 3] = [1, 8, 40];
/// Transfers per (length, clock) combo. 9 combos x this ~= "~10^5" from the
/// issue's acceptance criteria.
const ITERS_PER_COMBO: u32 = 10_000;
/// Progress line cadence within a combo, so a multi-minute combo (3169 B @
/// 1 MHz) is still visibly alive rather than silent for minutes.
const PROGRESS_EVERY: u32 = 2_000;

/// Set by the ISR: `IN_SUC_EOF` was already 1 when `TRANS_DONE` fired.
static EOF_ALREADY_SET: AtomicU32 = AtomicU32::new(0);
/// Set by the ISR: `IN_SUC_EOF` was still 0 when `TRANS_DONE` fired — the
/// race the issue is asking about, if this is ever nonzero.
static EOF_NOT_YET_SET: AtomicU32 = AtomicU32::new(0);
/// How many times the ISR actually ran — a sanity cross-check against the
/// iteration count, independent of the EOF question itself.
static ISR_FIRED: AtomicU32 = AtomicU32::new(0);

/// Bound to SPI2's `SPI_TRANS_DONE` — the only interrupt source enabled.
/// Reads the GDMA RX channel's raw `IN_SUC_EOF` bit first, before anything
/// else observes or perturbs peripheral state, then clears `trans_done` so
/// the level-triggered line deasserts.
#[handler(priority = Priority::Priority1)]
fn spi2_trans_done_handler() {
    let suc_eof = DMA::regs().ch(DMA_CHANNEL).in_int().raw().read().in_suc_eof().bit();
    if suc_eof {
        EOF_ALREADY_SET.fetch_add(1, Ordering::Relaxed);
    } else {
        EOF_NOT_YET_SET.fetch_add(1, Ordering::Relaxed);
    }
    ISR_FIRED.fetch_add(1, Ordering::Relaxed);
    SPI2::regs().dma_int_clr().write(|w| w.trans_done().clear_bit_by_one());
}

/// Let the BSP console's drain task run between blocking stretches.
async fn drain() {
    embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
    let p = board::init();

    let tg0 = TimerGroup::new(p.TIMG0);
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    mem::init_heap(HeapProfile::Default, Some(p.PSRAM));
    esp_rtos::start(AnyTimer::from(tg0.timer0), sw_int.software_interrupt0);
    let _console = shim::init_console(_spawner, board::console_serial(p.USB_DEVICE));

    log::info!("#16 GDMA IN_SUC_EOF-vs-TRANS_DONE timing probe on {}", board::NAME);
    drain().await;

    let (sclk, mosi, cs) = (AnyPin::from(p.GPIO5), AnyPin::from(p.GPIO6), AnyPin::from(p.GPIO7));
    // SAFETY: same self-loopback trick as spi_cmd_probe.rs — MOSI driven out,
    // its own clone read back in as MISO. Only free M-Bus pads are touched.
    let miso = unsafe { mosi.clone_unchecked() };

    let mut spi_dma = SpiMaster::new(
        p.SPI2,
        SpiConfig::default().with_frequency(Rate::from_mhz(1)).with_mode(Mode::_0),
    )
    .expect("SPI2 master init")
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_miso(miso)
    .with_cs(cs)
    .with_dma(p.DMA_CH0);

    spi_dma.set_interrupt_handler(spi2_trans_done_handler);
    spi_dma.listen(SpiInterrupt::TransferDone);

    let (rx_buffer, rx_descriptors, _tx_buffer, _tx_descriptors) = dma_buffers!(3169);
    let mut rx = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("rx dma buf");

    let mut any_not_set = false;
    let mut total_transfers = 0u64;
    let mut total_already_set = 0u64;
    let mut total_not_set = 0u64;

    for &len in &LENGTHS {
        for &mhz in &CLOCKS_MHZ {
            spi_dma
                .apply_config(&SpiConfig::default().with_frequency(Rate::from_mhz(mhz)).with_mode(Mode::_0))
                .expect("apply_config");

            EOF_ALREADY_SET.store(0, Ordering::Relaxed);
            EOF_NOT_YET_SET.store(0, Ordering::Relaxed);
            ISR_FIRED.store(0, Ordering::Relaxed);

            drain().await;
            log::info!("combo len={len:<4} clock={mhz:>2}MHz: running {ITERS_PER_COMBO} transfers...");

            for i in 0..ITERS_PER_COMBO {
                rx.as_mut_slice()[..len].fill(0xEE);
                rx.set_length(len);
                match spi_dma.read(len, rx) {
                    Ok(xfer) => {
                        let (s, r) = xfer.wait();
                        spi_dma = s;
                        rx = r;
                    }
                    Err((e, s, r)) => {
                        log::error!("len={len} clock={mhz}MHz iter={i}: read failed: {e:?}");
                        spi_dma = s;
                        rx = r;
                        break;
                    }
                }
                if i > 0 && i % PROGRESS_EVERY == 0 {
                    log::info!("  ... {i}/{ITERS_PER_COMBO}");
                    drain().await;
                }
            }

            // Let a just-fired ISR for the final transfer of this combo
            // finish draining before reading the tallies it wrote.
            drain().await;

            let set = EOF_ALREADY_SET.load(Ordering::Relaxed);
            let notset = EOF_NOT_YET_SET.load(Ordering::Relaxed);
            let fired = ISR_FIRED.load(Ordering::Relaxed);
            if fired != ITERS_PER_COMBO {
                log::warn!(
                    "len={len} clock={mhz}MHz: isr_fired={fired} != iterations={ITERS_PER_COMBO} (some transfers produced no TRANS_DONE interrupt)"
                );
            }
            if notset > 0 {
                any_not_set = true;
            }
            total_transfers += fired as u64;
            total_already_set += set as u64;
            total_not_set += notset as u64;
            log::info!(
                "RESULT len={len:<4} clock={mhz:>2}MHz: isr_fired={fired} suc_eof_already_set={set} suc_eof_NOT_yet_set={notset}"
            );
            drain().await;
        }
    }

    drain().await;
    log::info!(
        "#16 sweep done: {total_transfers} transfers, already_set={total_already_set} not_yet_set={total_not_set}"
    );
    if any_not_set {
        log::error!(
            "#16 verdict: IN_SUC_EOF is NOT always set when TRANS_DONE fires — a driver re-arming from the ISR must chain on in_suc_eof, not trans_done alone"
        );
    } else {
        log::info!(
            "#16 verdict: IN_SUC_EOF was set 100% of the time TRANS_DONE fired, across all {} combos ({total_transfers} transfers) — trans_done is a safe DMA-RX re-arm point on this chip",
            LENGTHS.len() * CLOCKS_MHZ.len()
        );
    }

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(5)).await;
    }
}
