// SPDX-License-Identifier: MIT OR Apache-2.0
//! #17 (ESP32-S3): characterise SPI2/GDMA fault terminations — can a
//! transfer wedge `SPI_USR` with no interrupt, and what recovers it?
//!
//! **On-chip, no external hardware.** SPI2 runs as master clocking into an
//! unconnected bus (MOSI on a free M-Bus pad, nothing listens) — no slave
//! needed, since the point is to fault the *supply side* (GDMA -> SPI FIFO),
//! not the data itself.
//!
//! **Part 1 — induce it.** Start a long (32000 B) DMA write at a slow clock
//! (100 kHz, ~2.56 s full length), wait 80 ms (comfortably mid-transfer),
//! then forcibly halt the GDMA outbound linked list
//! (`GDMA_OUT_LINK_CH0.OUTLINK_STOP`) — the same primitive esp-hal's own
//! `stop_transfer()` uses, just invoked mid-flight instead of at a clean
//! boundary. If the SPI shift register drains its buffered bytes faster
//! than nothing arrives, `SPI_DMA_OUTFIFO_EMPTY_ERR`'s documented behaviour
//! ("SPI will stop in master mode") predicts `SPI_CMD.SPI_USR` sticks set
//! with the SPI FSM frozen mid-word — and, per ESP-IDF's driver only ever
//! enabling `trans_done`, nothing tells the CPU.
//!
//! **Part 2 — recover it**, only if Part 1 actually wedged: a 3-rung ladder,
//! stopping at the first rung that clears `SPI_USR`, then verifying the
//! peripheral is genuinely usable again (not just registers reading zero)
//! with one small real transfer:
//!   1. Shrink `MS_DATA_BITLEN` to 7 (1 byte) + pulse `CMD.UPDATE` — esp-hal's
//!      own `abort_transfer()` trick (`spi/master/dma.rs`, `abort_transfer`/
//!      `configure_datalen`), on the theory that satisfying the running bit
//!      counter's exit condition lets the FSM terminate normally.
//!   2. `SPI_SLAVE.SPI_SOFT_RESET` (bit 27) — TRM: "reset the spi clock
//!      line, cs line, and data lines". ESP-IDF only calls this from
//!      `spi_ll_slave_reset()`; master-mode use is unverified upstream.
//!   3. `SYSTEM_PERIP_RST_EN0.SPI2_RST` 1->0 — a full peripheral reset,
//!      guaranteed to work but wipes all configuration (the raw-register
//!      equivalent of `PeripheralClockControl::reset`, esp-hal's own
//!      internal recovery primitive, which is `pub(crate)` and not
//!      reachable from here).
//!
//! The transfer handle from the induced-wedge write is deliberately never
//! `.wait()`ed or dropped normally: like `spi_cmd_probe.rs`'s stalled-slave
//! path, both spin on `is_done()` forever if the peripheral really is
//! wedged. It is `core::mem::forget`'d instead; all recovery and
//! verification happens through raw registers and freshly `steal()`'d
//! peripheral handles.
//!
//! Pad (M-Bus, free per #15, unconnected on a bare board):
//!   CoreS3  SCLK=GPIO5  MOSI=GPIO6  CS=GPIO7
//!
//! Build: `--bin spi2_gdma_fault_probe --no-default-features --features cores3`
//! (ESP32-S3 only).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

use crate::common::{board, shim};
use embassy_executor::Spawner;
use esp_hal::{
    dma::DmaTxBuf,
    dma_buffers,
    gpio::AnyPin,
    interrupt::software::SoftwareInterruptControl,
    peripherals::{DMA, DMA_CH0, SPI2, SYSTEM},
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi as SpiMaster},
    },
    time::Rate,
    timer::{AnyTimer, timg::TimerGroup},
};
use m5stack_core::mem::{self, HeapProfile};

m5stack_core::app_desc!();

/// `p.DMA_CH0` — the only GDMA channel this probe uses.
const DMA_CHANNEL: usize = 0;
/// Long enough at a slow clock to give a wide, comfortable window to inject
/// the fault well before the transfer would finish naturally.
const TX_LEN: usize = 32_000;
/// Slow enough that TX_LEN takes ~2.56 s, so an 80 ms injection delay is
/// deep inside the transfer with a huge safety margin either side.
const SCLK_KHZ: u32 = 100;
/// How long to wait after starting before stopping the outlink.
const INJECT_DELAY_MS: u64 = 80;
/// Bound on the rung-1 `CMD.UPDATE` poll — this is the thing we're testing,
/// so it must not be able to hang the probe itself if it never clears.
const UPDATE_POLL_BUDGET: u32 = 500_000;

fn spi_usr_set() -> bool {
    SPI2::regs().cmd().read().usr().bit()
}

async fn drain() {
    embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
}

/// Part 1's register dump: SPI_USR, SPI's own DMA interrupt-raw bits, and
/// every bit of both GDMA in/out raw-interrupt registers for the channel.
fn dump_part1_diagnostics(tag: &str) {
    let usr = spi_usr_set();
    let dma_int = SPI2::regs().dma_int_raw().read();
    let out_int = DMA::regs().ch(DMA_CHANNEL).out_int().raw().read();
    let in_int = DMA::regs().ch(DMA_CHANNEL).in_int().raw().read();

    log::info!("[{tag}] SPI_CMD.SPI_USR = {}", usr as u8);
    log::info!(
        "[{tag}] SPI_DMA_INT_RAW = {:#010x}  trans_done={} infifo_full_err={} outfifo_empty_err={} mst_rx_afifo_wfull_err={} mst_tx_afifo_rempty_err={}",
        dma_int.bits(),
        dma_int.trans_done().bit() as u8,
        dma_int.dma_infifo_full_err().bit() as u8,
        dma_int.dma_outfifo_empty_err().bit() as u8,
        dma_int.mst_rx_afifo_wfull_err().bit() as u8,
        dma_int.mst_tx_afifo_rempty_err().bit() as u8,
    );
    log::info!(
        "[{tag}] GDMA_OUT_INT_RAW_CH{DMA_CHANNEL} = {:#010x}  out_done={} out_eof={} out_dscr_err={} out_total_eof={} outfifo_ovf_l1={} outfifo_udf_l1={} outfifo_ovf_l3={} outfifo_udf_l3={}",
        out_int.bits(),
        out_int.out_done().bit() as u8,
        out_int.out_eof().bit() as u8,
        out_int.out_dscr_err().bit() as u8,
        out_int.out_total_eof().bit() as u8,
        out_int.outfifo_ovf_l1().bit() as u8,
        out_int.outfifo_udf_l1().bit() as u8,
        out_int.outfifo_ovf_l3().bit() as u8,
        out_int.outfifo_udf_l3().bit() as u8,
    );
    log::info!(
        "[{tag}] GDMA_IN_INT_RAW_CH{DMA_CHANNEL}  = {:#010x}  in_done={} in_suc_eof={} in_err_eof={} in_dscr_err={} in_dscr_empty={} infifo_full_wm={} infifo_ovf_l1={} infifo_udf_l1={} infifo_ovf_l3={} infifo_udf_l3={}",
        in_int.bits(),
        in_int.in_done().bit() as u8,
        in_int.in_suc_eof().bit() as u8,
        in_int.in_err_eof().bit() as u8,
        in_int.in_dscr_err().bit() as u8,
        in_int.in_dscr_empty().bit() as u8,
        in_int.infifo_full_wm().bit() as u8,
        in_int.infifo_ovf_l1().bit() as u8,
        in_int.infifo_udf_l1().bit() as u8,
        in_int.infifo_ovf_l3().bit() as u8,
        in_int.infifo_udf_l3().bit() as u8,
    );
}

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
    let p = board::init();

    let tg0 = TimerGroup::new(p.TIMG0);
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    mem::init_heap(HeapProfile::Default);
    esp_rtos::start(AnyTimer::from(tg0.timer0), sw_int.software_interrupt0);
    let _console = shim::init_console(_spawner, board::console_serial(p.USB_DEVICE));

    log::info!("#17 SPI2/GDMA fault-termination probe on {}", board::NAME);
    drain().await;

    // SPI2/DMA_CH0/pins are never taken from `p` — every round (including
    // the first) claims them via `steal()` uniformly instead, since a
    // wedged round ends by leaking its driver (see below) rather than
    // returning it. `main` never returns, so `p`'s untouched fields simply
    // stay alive, unused, for the process lifetime.

    // Repeats indefinitely, same reason `spi_cmd_probe.rs` sweeps: the
    // USB-Serial-JTAG CDC console drops across a `probe-rs reset`, so a
    // one-shot ~3s report is easy to miss entirely. Any capture window now
    // catches a full round, and repeats also show whether recovery is
    // reliable across cycles, not just a single lucky one.
    let mut round = 0u32;
    loop {
        round += 1;
        drain().await;
        log::info!("=== round {round} ===");
        run_round(round).await;
        log::info!("=== round {round} done ===");
        embassy_time::Timer::after(embassy_time::Duration::from_secs(3)).await;
    }
}

async fn run_round(round: u32) {
    // SAFETY: see the `mem::forget(p)` note in `main` — this binary is the
    // sole, exclusive owner of SPI2/DMA_CH0/GPIO5-7 for its entire run.
    let (sclk, mosi, cs) = unsafe { (AnyPin::steal(5), AnyPin::steal(6), AnyPin::steal(7)) };
    let spi2_peri = unsafe { SPI2::steal() };
    let dma_ch = unsafe { DMA_CH0::steal() };

    let spi_dma = SpiMaster::new(
        spi2_peri,
        SpiConfig::default()
            .with_frequency(Rate::from_khz(SCLK_KHZ))
            .with_mode(Mode::_0),
    )
    .expect("SPI2 master init")
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_cs(cs)
    .with_dma(dma_ch);

    let (_rx_buffer, _rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(TX_LEN);
    tx_buffer.fill(0xA5);
    let tx = DmaTxBuf::new(tx_descriptors, tx_buffer).expect("tx dma buf");

    log::info!("Part 1: inducing a TX underrun via GDMA_OUT_LINK_CH{DMA_CHANNEL}.OUTLINK_STOP");
    log::info!(
        "  starting a {TX_LEN}-byte DMA write @ {SCLK_KHZ} kHz (~{} ms full length), injecting at {INJECT_DELAY_MS} ms",
        (TX_LEN as u64 * 8) / SCLK_KHZ as u64
    );
    drain().await;

    let xfer = match spi_dma.write(TX_LEN, tx) {
        Ok(x) => x,
        Err((e, _s, _t)) => {
            log::error!("round {round}: write start failed: {e:?}");
            return;
        }
    };

    embassy_time::Timer::after(embassy_time::Duration::from_millis(INJECT_DELAY_MS)).await;

    log::warn!("injecting fault now: OUTLINK_STOP on GDMA channel {DMA_CHANNEL}");
    DMA::regs()
        .ch(DMA_CHANNEL)
        .out_link()
        .modify(|_, w| w.outlink_stop().set_bit());

    // Let the fault actually propagate (FIFO drain past whatever margin
    // GDMA had pre-buffered) before reading anything.
    embassy_time::Timer::after(embassy_time::Duration::from_millis(20)).await;

    dump_part1_diagnostics("post-inject");

    let wedged = spi_usr_set();
    if wedged {
        log::error!(
            "Part 1 verdict: SPI_USR IS stuck set with no fault interrupt raised (trans_done={}) -- confirmed a silent wedge",
            SPI2::regs().dma_int_raw().read().trans_done().bit() as u8
        );
    } else {
        log::info!(
            "Part 1 verdict: SPI_USR cleared on its own -- the injected stop did not wedge the peripheral"
        );
    }

    // The Rust-level transfer handle must not be `.wait()`ed or dropped
    // normally from here: if genuinely wedged, both spin on `is_done()`
    // forever (same hazard `spi_cmd_probe.rs` documents for a stalled
    // slave). Leak it; everything from here on is raw registers and fresh
    // `steal()`d handles.
    core::mem::forget(xfer);

    if !wedged {
        log::info!("round {round}: no wedge reproduced, nothing to test recovery against.");
        return;
    }

    log::info!("Part 2: recovery ladder");
    drain().await;

    log::info!("  rung 1: MS_DATA_BITLEN <- 7, pulse CMD.UPDATE (esp-hal's abort_transfer trick)");
    SPI2::regs()
        .ms_dlen()
        .write(|w| unsafe { w.ms_data_bitlen().bits(7) });
    SPI2::regs().cmd().modify(|_, w| w.update().set_bit());
    let mut spins = 0u32;
    while SPI2::regs().cmd().read().update().bit_is_set() && spins < UPDATE_POLL_BUDGET {
        spins += 1;
    }
    let update_cleared = spins < UPDATE_POLL_BUDGET;
    drain().await;
    let usr_after_1 = spi_usr_set();
    log::info!(
        "  rung 1 result: CMD.UPDATE cleared={update_cleared} (after {spins} spins, budget {UPDATE_POLL_BUDGET}) SPI_USR={}",
        usr_after_1 as u8
    );

    let mut winning_rung = None;
    if !usr_after_1 {
        winning_rung = Some(1);
    } else {
        log::info!("  rung 2: SPI_SLAVE.SPI_SOFT_RESET (bit 27)");
        SPI2::regs().slave().modify(|_, w| w.soft_reset().set_bit());
        drain().await;
        let usr_after_2 = spi_usr_set();
        log::info!("  rung 2 result: SPI_USR={}", usr_after_2 as u8);
        if !usr_after_2 {
            winning_rung = Some(2);
        } else {
            log::info!("  rung 3: SYSTEM_PERIP_RST_EN0.SPI2_RST 1 -> 0 (full peripheral reset)");
            SYSTEM::regs()
                .perip_rst_en0()
                .modify(|_, w| w.spi2_rst().set_bit());
            SYSTEM::regs()
                .perip_rst_en0()
                .modify(|_, w| w.spi2_rst().clear_bit());
            drain().await;
            let usr_after_3 = spi_usr_set();
            log::info!("  rung 3 result: SPI_USR={}", usr_after_3 as u8);
            if !usr_after_3 {
                winning_rung = Some(3);
            }
        }
    }

    match winning_rung {
        Some(rung) => log::info!("Part 2 verdict: rung {rung} cleared SPI_USR"),
        None => log::error!(
            "Part 2 verdict: SPI_USR is STILL stuck after all three rungs -- no known register-level recovery"
        ),
    }

    if let Some(rung) = winning_rung {
        log::info!(
            "Verifying the peripheral is actually usable (not just SPI_USR=0): one small real transfer"
        );
        drain().await;
        // SAFETY: the original Spi/DMA/pin handles were leaked above
        // (mem::forget), never dropped -- these are the only live Rust
        // references to this hardware, and nothing else touches it
        // concurrently in this single-threaded probe.
        let (sclk2, mosi2, cs2) = unsafe { (AnyPin::steal(5), AnyPin::steal(6), AnyPin::steal(7)) };
        let miso2 = unsafe { mosi2.clone_unchecked() };
        let spi2_peri = unsafe { SPI2::steal() };
        let _unused_dma_ch = unsafe { DMA_CH0::steal() }; // not used by a non-DMA transfer; steal()'d to document it's free again.

        let mut verify = SpiMaster::new(
            spi2_peri,
            SpiConfig::default()
                .with_frequency(Rate::from_khz(SCLK_KHZ))
                .with_mode(Mode::_0),
        )
        .expect("SPI2 re-init after recovery")
        .with_sck(sclk2)
        .with_mosi(mosi2)
        .with_miso(miso2)
        .with_cs(cs2);

        let mut data = [0x5Au8, 0xA5, 0x3C, 0xC3];
        match verify.transfer(&mut data) {
            Ok(()) => {
                log::info!(
                    "Verify OK after rung {rung}: loopback wrote=[5a,a5,3c,c3] read={data:02x?}"
                )
            }
            Err(e) => log::error!(
                "Verify FAILED after rung {rung}: transfer error {e:?} -- SPI_USR cleared but the peripheral is not actually usable"
            ),
        }
    }
}
