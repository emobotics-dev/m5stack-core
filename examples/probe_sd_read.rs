// SPDX-License-Identifier: MIT OR Apache-2.0
//! Does an SD block read work *before* the display is brought up?
//!
//! `sd.rs` reads the card only after `finish`/`finish_sd` has initialised the
//! panel, i.e. after a burst of TX DMA on the shared SPI2 bus. On Fire27
//! (ESP32/PDMA) every 512-byte read then returns zeros while single-byte
//! command transfers still work — and alternator-regulator carries a note that
//! the display-first ordering "reliably wedges" on exactly this path.
//!
//! This probe does the opposite order and nothing else: bring the bus up, run
//! the SD power-up idle, read the card size and LBA 0 on the still-exclusive
//! bus, then transfer raw bytes at a range of sizes. The panel is never
//! initialised.
//!
//! Reading the block as `Ok(())` is meaningful: `init()` enables CRC (CMD59)
//! and `read_data` rejects a mismatch, so a successful all-zero read means the
//! card really transmitted zeros — a blank card, not a broken bus. The raw
//! transfers separate "the wire is dead" from "the data is zero".
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

#[path = "common/mod.rs"]
mod common;

use block_device_driver::BlockDevice;
use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Delay, Timer};
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use static_cell::make_static;

m5stack_core::app_desc!();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    common::boot!(spawner, board, Default);

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(512, 512);
    let dma_rx = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("DMA rx");
    let dma_tx = DmaTxBuf::new(tx_descriptors, tx_buffer).expect("DMA tx");

    let (parts, card_cs) = board.spi2.into_parts(dma_rx, dma_tx).expect("SPI2 parts");
    let mut bus = parts.bus; // display never initialised — `parts` dropped here
    let mut cs = card_cs;

    for attempt in 0..5u32 {
        match sdspi::sd_init(&mut bus, &mut cs).await {
            Ok(()) => break,
            Err(e) => log::warn!("sd_init {}: {:?}", attempt, e),
        }
        Timer::after_millis(10).await;
    }

    let bus: &'static mut Mutex<CriticalSectionRawMutex, _> = make_static!(Mutex::new(bus));
    let dev = SpiDeviceWithConfig::new(bus, cs, m5stack_core::board::spi2::sd_init_config());
    let mut sd = sdspi::SdSpi::<_, _, aligned::A1>::new(dev, Delay);
    log::info!("probe: init={}", sd.init().await.is_ok());

    match sd.size().await {
        Ok(sz) => log::info!(
            "probe: card size = {} bytes ({} MiB)",
            sz,
            sz / (1024 * 1024)
        ),
        Err(e) => log::warn!("probe: size err {:?}", e),
    }

    let mut blk = [aligned::Aligned::<aligned::A1, [u8; 512]>([0xAAu8; 512]); 1];
    match sd.read(0, &mut blk).await {
        Ok(()) => {
            let b = &blk[0][..];
            log::info!(
                "probe: lba0 untouched(0xAA)={}/512 head={:02x?} sig={:02x?}",
                b.iter().filter(|&&x| x == 0xAA).count(),
                &b[..8],
                &b[510..512]
            );
        }
        Err(e) => log::warn!("probe: read err {:?}", e),
    }

    // Raw SPI transfers at increasing size. An idle, selected card holds MISO
    // high, so each of these should read back 0xFF. Any size that comes back
    // 0x00 is the DMA RX path failing, independent of the SD protocol.
    {
        use embedded_hal_async::spi::{Operation, SpiDevice};
        let dev = sd.spi();
        for n in [8usize, 64, 128, 256, 384, 512] {
            let mut buf = [0xAAu8; 512];
            match dev
                .transaction(&mut [Operation::TransferInPlace(&mut buf[..n])])
                .await
            {
                Ok(()) => {
                    let ff = buf[..n].iter().filter(|&&x| x == 0xFF).count();
                    let zero = buf[..n].iter().filter(|&&x| x == 0x00).count();
                    let aa = buf[..n].iter().filter(|&&x| x == 0xAA).count();
                    log::info!(
                        "raw n={:3}: 0xFF={:3} 0x00={:3} untouched={:3}",
                        n,
                        ff,
                        zero,
                        aa
                    );
                }
                Err(_) => log::warn!("raw n={}: transfer error", n),
            }
            Timer::after_millis(5).await;
        }
    }

    core::future::pending::<()>().await
}
