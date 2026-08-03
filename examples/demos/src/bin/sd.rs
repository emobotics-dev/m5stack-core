// SPDX-License-Identifier: MIT OR Apache-2.0
//! SD card over the shared SPI2 bus: mount the FAT filesystem and list the root
//! directory (read-only — it never writes, so it won't touch existing logs).
//!
//! This is the one demo that exercises `board::spi2::finish_sd` — the display +
//! SD shared bus, including the CoreS3 GPIO35 MISO/DC mux. The SD *driver*
//! (`sdspi` + `embedded-fatfs`) is an example dependency (the fork isn't on
//! crates.io); the BSP stops at the shared bus + a generic-CS `SpiDevice`.
//!
//! Build: `cargo +esp run --release -p demos --bin sd --features sd` (Fire27) or
//! add `--no-default-features --features cores3,sd --target xtensa-esp32s3-none-elf`.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

use common::{STRIP_BYTES, draw_panel};
use demos::board::NAME;
use embassy_executor::Spawner;
use embassy_time::{Delay, Duration, Timer};
use esp_hal::{
    dma::{DmaRxBuf, DmaTxBuf},
    dma_buffers,
    spi::{Mode, master::Config as SpiConfig},
    time::Rate,
};
use m5stack_core::board::spi2::CardPresence;
use static_cell::make_static;

// Panic handler + log/console transport come from the BSP (the panic-handler +
// console-serial features); the app descriptor is the one line the binary keeps.
m5stack_core::app_desc!();

/// Bounded SD-init attempts — a dead/absent card must never hang the demo.
const SD_RETRIES: u32 = 5;

/// Card presence handed to `finish_sd`. Flip to `ForceAbsent` to exercise the
/// SD-absent degrade path with a card physically inserted (HIL `:nosd`).
const PRESENCE: CardPresence = CardPresence::Detect;


/// Peek sector 0 and, if it is an MBR, return the `(start_lba, sector_count)` of
/// the first FAT partition. Returns `None` for a card with no MBR partition
/// table (a "superfloppy" with the FAT volume at sector 0), which the caller
/// then mounts whole.
///
/// Heuristic kept deliberately strict so a superfloppy boot sector (which also
/// ends in the 0x55AA signature, but whose bytes 446.. are boot code, not a
/// partition table) isn't misread as MBR: each candidate entry's status byte
/// must be 0x00/0x80 and its type one of the FAT IDs, with non-zero LBA/size.
async fn first_fat_partition<IO>(bs: &mut IO) -> Option<(u32, u32)>
where
    IO: embedded_io_async::Read + embedded_io_async::Seek,
{
    use embedded_io_async::SeekFrom;
    bs.seek(SeekFrom::Start(0)).await.ok()?;
    let mut mbr = [0u8; 512];
    bs.read_exact(&mut mbr).await.ok()?;
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        return None;
    }

    // VBR-first guard (the mirror of embedded-fatfs's MBR check): a superfloppy
    // (FAT boot sector at sector 0) also ends in 0x55AA, and its boot code at
    // offset 446 can coincidentally look like an MBR partition entry — which
    // would slice us to a bogus offset and fail to mount a good card. A real boot
    // sector is unambiguous: a jump opcode at byte 0 and a power-of-two
    // bytes_per_sector (512..=4096) at offset 11. If that holds it's a
    // superfloppy → mount whole, don't scan for partitions.
    let bytes_per_sector = u16::from_le_bytes([mbr[11], mbr[12]]);
    let looks_like_vbr = (mbr[0] == 0xEB || mbr[0] == 0xE9)
        && bytes_per_sector.count_ones() == 1
        && (512..=4096).contains(&bytes_per_sector);
    if looks_like_vbr {
        return None;
    }

    let le32 = |s: &[u8]| u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
    for i in 0..4 {
        let e = &mbr[446 + i * 16..446 + i * 16 + 16];
        let status = e[0];
        if status != 0x00 && status != 0x80 {
            return None; // not a valid MBR status byte → treat as non-MBR
        }
        // FAT12/FAT16/FAT16B/FAT32(CHS)/FAT32(LBA)/FAT16(LBA).
        let is_fat = matches!(e[4], 0x01 | 0x04 | 0x06 | 0x0B | 0x0C | 0x0E);
        let lba = le32(&e[8..12]);
        let sectors = le32(&e[12..16]);
        if is_fat && lba != 0 && sectors != 0 {
            return Some((lba, sectors));
        }
    }
    None
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    demos::boot!(spawner, board, Default);

    // CoreS3: reset/power the panel over I2C before the display init in finish().
    #[cfg(feature = "cores3")]
    {
        let i2c = demos::board::init_i2c_shared(board.i2c0);
        m5stack_core::board::cores3::power_display_reset(i2c).await;
    }

    // DMA buffers for the shared bus: RX one SD block (512 B), TX one display
    // strip. `into_parts` keeps the bus exclusive for the SD pre-init clocks;
    // `finish` then shares it + brings up the display.
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(512, STRIP_BYTES);
    let dma_rx = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("DMA rx");
    let dma_tx = DmaTxBuf::new(tx_descriptors, tx_buffer).expect("DMA tx");

    let (parts, card_cs) = board.spi2.into_parts(dma_rx, dma_tx).expect("SPI2 parts");

    // The BSP owns SD bring-up: `finish_sd` runs the >=74-clock power-up idle on
    // the exclusive bus, brings the display up unconditionally (CoreS3: GPIO35 is
    // re-muxed to MISO here), and returns a presence-resolved card device. No
    // manual pre-init loop; `PRESENCE` can force the absent path with a card in.
    let (mut driver, prepared) =
        parts.finish_sd(card_cs, PRESENCE).await.expect("display init");

    // Read-only `ls` into display lines (and the log). Done before any draw, so
    // GPIO35 stays MISO on CoreS3 for the whole SD I/O (no DC writes interleave).
    let mut lines: alloc::vec::Vec<String> = alloc::vec::Vec::new();
    {
        // With `finish_sd`, the >=74-clock pre-init already ran in the BSP; a
        // failed `sd.init()` is now the sole SD-absent signal (real-absent and
        // `ForceAbsent` both land here — one degrade path).
        let mut sd = sdspi::SdSpi::<_, _, aligned::A1>::new(prepared.into_inner(), Delay);
        let mut init_ok = false;
        for attempt in 0..SD_RETRIES {
            if sd.init().await.is_ok() {
                init_ok = true;
                break;
            }
            log::warn!("sd card init attempt {}", attempt);
            Timer::after_millis(5).await;
        }
        if !init_ok {
            lines.push("no card / init failed".to_string());
        } else {
            // Raise the device clock from the 400 kHz init rate to a safe run rate.
            sd.spi().set_config(
                SpiConfig::default()
                    .with_frequency(Rate::from_khz(10_000))
                    .with_mode(Mode::_0),
            );

            // embedded-fatfs mounts a FAT *volume*, not a whole disk: it expects a
            // FAT boot sector (BPB) at byte 0 of the stream it's given. Most SD
            // cards ship MBR-partitioned (sector 0 = partition table, the FAT
            // volume starts inside partition 1) — handing that sector 0 straight
            // to `FileSystem::new` parses the zeroed MBR bootstrap as a BPB and
            // dies with "bytes_per_sector got 0" / CorruptedFileSystem. So: peek
            // sector 0, find the first FAT partition, and mount through a
            // `StreamSlice` windowed onto it. Cards formatted as a "superfloppy"
            // (FAT right at sector 0, no MBR) fall back to a whole-device slice.
            let mut bs = block_device_adapters::BufStream::<_, 512>::new(sd);
            let (start, end) = match first_fat_partition(&mut bs).await {
                Some((lba, sectors)) => {
                    log::info!("[sd] MBR FAT partition @ LBA {} ({} sectors)", lba, sectors);
                    (lba as u64 * 512, (lba as u64 + sectors as u64) * 512)
                }
                None => {
                    log::info!("[sd] no MBR partition table; trying superfloppy (FAT @ sector 0)");
                    (0, u64::MAX)
                }
            };
            // `StreamSlice::new` seeks the inner stream to `start`, which also
            // rewinds the position we moved while peeking sector 0.
            // Per-step logs (the console timestamps each line) show where the time
            // goes — mount vs the optional free-space scan vs the root listing.
            log::info!("[sd] mounting FAT volume...");
            match block_device_adapters::StreamSlice::new(bs, start, end).await {
                Ok(slice) => match embedded_fatfs::FileSystem::new(
                    slice,
                    embedded_fatfs::FsOptions::new(),
                )
                .await
                {
                    Ok(fs) => {
                        log::info!("[sd] mounted");
                        // Free space, only when it costs nothing: `free_clusters_hint()`
                        // reads the cached FSInfo count (O(1), no scan). We deliberately
                        // do NOT call `fs.stats()`, which would scan the whole FAT
                        // (O(card size), tens of seconds on a big card — the #50 "hang").
                        // If the hint is unknown, we just skip the line.
                        match fs.free_clusters_hint() {
                            Some(free) => {
                                let total = fs.total_clusters();
                                let kib = |c: u32| (c as u64 * fs.cluster_size() as u64) / 1024;
                                lines.push(format!("free {} / {} KiB", kib(free), kib(total)));
                            }
                            None => log::info!("[sd] free space unknown (no FSInfo hint; not scanning)"),
                        }
                        lines.push("root:".to_string());
                        log::info!("[sd] listing root...");
                        // Scope the dir iterator (it borrows `fs`) so it is dropped
                        // before `fs.unmount()` moves `fs`.
                        {
                            let root = fs.root_dir();
                            let mut it = root.iter();
                            let mut n = 0u32;
                            while let Some(entry) = it.next().await {
                                match entry {
                                    Ok(e) if e.is_dir() => {
                                        lines.push(format!("  {}/", e.file_name()))
                                    }
                                    Ok(e) => {
                                        lines.push(format!("  {}  {} B", e.file_name(), e.len()))
                                    }
                                    Err(e) => {
                                        log::warn!("dir entry error: {:?}", e);
                                        break;
                                    }
                                }
                                n += 1;
                                // Cap the on-screen list; the full count is logged.
                                if n >= 10 {
                                    lines.push("  ...".to_string());
                                    break;
                                }
                            }
                            log::info!("[sd] root listed ({} entries shown)", n);
                        }
                        let _ = fs.unmount().await;
                    }
                    Err(e) => {
                        log::warn!("mount failed: {:?}", e);
                        lines.push("mount failed".to_string());
                    }
                },
                Err(e) => {
                    log::warn!("StreamSlice failed: {:?}", e);
                    lines.push("slice failed".to_string());
                }
            }
        }
    }

    for line in &lines {
        log::info!("[sd] {}", line);
    }

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    loop {
        draw_panel(&mut driver.display, &mut strip_buf[..], NAME, "SD ls", &refs).await;
        Timer::after(Duration::from_secs(5)).await;
    }
}
