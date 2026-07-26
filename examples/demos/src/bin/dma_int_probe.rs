// SPDX-License-Identifier: MIT OR Apache-2.0
//! #19 repro: does `clear_all()` really clear all GDMA channel interrupts?
//!
//! `DmaRxInterrupt`/`DmaTxInterrupt` can only name 5 of the S3 GDMA's 10 RX and
//! 4 of its 8 TX interrupt bits — the `INFIFO_*`/`OUTFIFO_*` overflow/underflow
//! and RX watermark bits have no variant. Since `clear_all()` is defined as
//! `clear(EnumSet::all())`, it can only clear what the enums can name: the name
//! promises exhaustive, the behaviour is not.
//!
//! **Getting a FIFO bit latched.** `IN_INT_RAW` is write-1-to-clear in practice
//! (an earlier attempt to set bits there by software latched nothing), so the
//! bit has to come from the hardware. `INFIFO_FULL_WM` (RX bit 5) is perfect:
//! its threshold `IN_CONF1.DMA_INFIFO_FULL_THRS` is configurable, so a low
//! threshold makes an ordinary transfer latch it and a high threshold stops it
//! latching again. esp-hal only ever `modify`s `IN_CONF1`, so the threshold
//! survives its setup.
//!
//! **The measurement.** `ChannelRx::do_prepare` calls `clear_all()` at the start
//! of every transfer, so a plain `Mem2Mem` transfer exercises the real path
//! through public API — no private hooks. The trick is that `clear_all()` runs
//! *before* the transfer, so the transfer re-sets whatever it sets; that is why
//! the threshold is raised first, making the measured bit one the second
//! transfer cannot re-latch.
//!
//! Each round:
//!   a. clear all 10/8 bits by hand (proves they *are* clearable) and confirm
//!      the registers read back zero;
//!   b. low threshold + transfer -> hardware latches `INFIFO_FULL_WM`;
//!   c. raise threshold, then transfer -> this transfer's `clear_all()` is the
//!      thing under test, and it cannot re-latch bit 5;
//!   d. control: clear by hand, transfer again at the high threshold, and
//!      confirm bit 5 stays clear — otherwise (c) proves nothing.
//!
//! Bit 5 surviving (c) while (d) is clean *is* the bug: the next transfer starts
//! with a stale FIFO error latched, and a handler that re-arms after
//! `clear_all()` sees it immediately.
//!
//! No pins, no external wiring — `Mem2Mem` needs neither.
//!
//! Build: `--bin dma_int_probe --no-default-features --features cores3`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

use demos::{board, shim};
use embassy_executor::Spawner;
use esp_hal::{
    dma::{BurstConfig, Mem2Mem},
    dma_descriptors,
    interrupt::software::SoftwareInterruptControl,
    timer::{AnyTimer, timg::TimerGroup},
};
use m5stack_core::mem::{self, HeapProfile};

m5stack_core::app_desc!();

/// The GDMA channel this probe drives and inspects.
const CH: usize = 0;
/// All ten RX / eight TX interrupt bits on the S3 GDMA.
const RX_ALL: u32 = 0x3FF;
const TX_ALL: u32 = 0x0FF;
/// `INFIFO_FULL_WM` is RX bit 5 — one of the five esp-hal cannot name.
const RX_FIFO_FULL_WM: u32 = 1 << 5;
/// Watermark thresholds: 0 trips on any data, 0xFFF never trips.
const THRS_LOW: u16 = 0;
const THRS_HIGH: u16 = 0xFFF;

/// Let the BSP console's drain task run between blocking steps.
async fn drain() {
    embassy_time::Timer::after(embassy_time::Duration::from_millis(50)).await;
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let p = board::init();

    let tg0 = TimerGroup::new(p.TIMG0);
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    mem::init_heap(HeapProfile::Default, Some(p.PSRAM));
    esp_rtos::start(AnyTimer::from(tg0.timer0), sw_int.software_interrupt0);
    let _console = shim::init_console(spawner, board::console_serial(p.USB_DEVICE));

    log::info!("#19 GDMA clear_all() completeness probe on {}", board::NAME);
    drain().await;

    let dma = esp_hal::peripherals::DMA::regs();

    // A Mem2Mem transfer is the cheapest thing that runs the real
    // `do_prepare -> clear_all()` path: no pins, no peripheral wiring.
    let (rx_descriptors, tx_descriptors) = dma_descriptors!(4096, 4096);
    let mut m2m = Mem2Mem::new(p.DMA_CH0, p.SPI2)
        .with_descriptors(rx_descriptors, tx_descriptors, BurstConfig::default())
        .expect("mem2mem descriptors");

    // Big enough that the RX FIFO genuinely fills during the transfer.
    let tx_buf = [0xA5u8; 4096];
    let mut rx_buf = [0u8; 4096];

    // Clear every bit by hand — including the five esp-hal cannot name. That
    // this works at all is the point: the bits are reachable, esp-hal just
    // never writes them.
    let clear_by_hand = || {
        dma.ch(CH).in_int().clr().write(|w| unsafe { w.bits(RX_ALL) });
        dma.ch(CH).out_int().clr().write(|w| unsafe { w.bits(TX_ALL) });
    };
    let set_thrs = |thrs: u16| {
        dma.ch(CH)
            .in_conf1()
            .modify(|_, w| unsafe { w.dma_infifo_full_thrs().bits(thrs) });
    };
    let run = |rx: &mut [u8], tx: &[u8], m2m: &mut esp_hal::dma::SimpleMem2Mem<'_, _>| {
        match m2m.start_transfer(rx, tx) {
            Ok(xfer) => {
                if let Err(e) = xfer.wait() {
                    log::error!("mem2mem transfer error: {:?}", e);
                }
            }
            Err(e) => log::error!("mem2mem start failed: {:?}", e),
        }
    };

    let mut round = 0u32;
    loop {
        round += 1;
        drain().await;
        log::info!("--- round {} ---", round);

        // (a) everything clearable?
        clear_by_hand();
        let rx0 = dma.ch(CH).in_int().raw().read().bits();
        let tx0 = dma.ch(CH).out_int().raw().read().bits();
        log::info!("a) manual clear-all  -> IN raw={:#05x} OUT raw={:#05x}", rx0, tx0);
        if rx0 != 0 || tx0 != 0 {
            log::error!("   bits not clearable by hand — harness invalid");
        }
        drain().await;

        // (b) latch INFIFO_FULL_WM via a low watermark.
        set_thrs(THRS_LOW);
        run(&mut rx_buf, &tx_buf, &mut m2m);
        let rx1 = dma.ch(CH).in_int().raw().read().bits();
        let latched = rx1 & RX_FIFO_FULL_WM != 0;
        log::info!("b) low thrs + xfer   -> IN raw={:#05x}  infifo_full_wm={}", rx1, latched as u8);
        if !latched {
            log::error!("   INFIFO_FULL_WM did not latch — cannot measure; harness invalid");
        }
        drain().await;

        // (c) the measurement: this transfer's internal clear_all() is under
        //     test, and the high threshold stops it re-latching bit 5.
        set_thrs(THRS_HIGH);
        run(&mut rx_buf, &tx_buf, &mut m2m);
        let rx2 = dma.ch(CH).in_int().raw().read().bits();
        let survived = rx2 & RX_FIFO_FULL_WM != 0;
        log::info!("c) high thrs + xfer  -> IN raw={:#05x}  infifo_full_wm={}", rx2, survived as u8);
        drain().await;

        // (d) control: with the threshold high and a clean start, bit 5 must
        //     stay clear — otherwise (c) says nothing.
        clear_by_hand();
        run(&mut rx_buf, &tx_buf, &mut m2m);
        let rx3 = dma.ch(CH).in_int().raw().read().bits();
        let control_dirty = rx3 & RX_FIFO_FULL_WM != 0;
        log::info!("d) control (clean)   -> IN raw={:#05x}  infifo_full_wm={}", rx3, control_dirty as u8);

        if !latched || control_dirty {
            log::error!("VERDICT: inconclusive (latch={} control_dirty={})", latched as u8, control_dirty as u8);
        } else if survived {
            log::error!("VERDICT: clear_all() INCOMPLETE — INFIFO_FULL_WM survived it  FAIL");
        } else {
            log::info!("VERDICT: clear_all() cleared INFIFO_FULL_WM  PASS");
        }
        drain().await;

        embassy_time::Timer::after(embassy_time::Duration::from_secs(3)).await;
    }
}
