// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 — display splash + button readout (minimal, no radio).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{AnyPin, Input, InputConfig, Pull},
    interrupt::software::SoftwareInterruptControl,
    ram,
    timer::timg::TimerGroup,
};
use esp_println as _;
use fire27::{Lcd, STRIP_BYTES, draw_demo, draw_status, init_display};
use log::info;
use static_cell::make_static;

esp_bootloader_esp_idf::esp_app_desc!();

#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    info!("custom_halt — resetting");
    loop {}
}

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // ESP32 DRAM is tight; keep the plain-DRAM heap small. NOTE: esp-alloc's
    // global heap holds at most 3 regions — these two plus the PSRAM region
    // (below) are exactly 3, so do NOT add a 4th `heap_allocator!`.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    esp_alloc::heap_allocator!(size: 64 * 1024);

    // --- PSRAM heap (Fire27 carries ~4 MB SPI PSRAM) ---
    // The ESP32 cannot DMA out of PSRAM, so the framebuffer stays in internal
    // RAM; PSRAM is registered here only to demonstrate the external heap.
    m5stack_core::mem::init_psram_heap(peripherals.PSRAM);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    let shared_spi = make_static!(None);
    let (mut display, mut bl): (Lcd, _) = init_display(
        shared_spi,
        peripherals.SPI2,
        AnyPin::from(peripherals.GPIO18),
        AnyPin::from(peripherals.GPIO23),
        AnyPin::from(peripherals.GPIO19),
        AnyPin::from(peripherals.GPIO14),
        AnyPin::from(peripherals.GPIO27),
        AnyPin::from(peripherals.GPIO33),
        AnyPin::from(peripherals.GPIO32),
    )
    .await;
    bl.set_high();
    info!("Display initialized");

    // Strip framebuffer in a static internal-RAM buffer (the ESP32 cannot DMA
    // from PSRAM), shared by the splash and the status loop — allocated once,
    // never leaked per frame.
    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);

    draw_demo(
        &mut display,
        &mut strip_buf[..],
        "Fire27",
        &["display demo"],
    )
    .await;
    info!("Demo drawn, entering button loop");

    let btn_left = Input::new(
        AnyPin::from(peripherals.GPIO39),
        InputConfig::default().with_pull(Pull::Up),
    );
    let btn_center = Input::new(
        AnyPin::from(peripherals.GPIO38),
        InputConfig::default().with_pull(Pull::Up),
    );
    let btn_right = Input::new(
        AnyPin::from(peripherals.GPIO37),
        InputConfig::default().with_pull(Pull::Up),
    );

    loop {
        let left = btn_left.is_low();
        let center = btn_center.is_low();
        let right = btn_right.is_low();
        draw_status(
            &mut display,
            &mut strip_buf[..],
            &[
                "Fire27 buttons",
                "",
                if left { "LEFT  : DOWN" } else { "LEFT  : up" },
                if center { "CENTER: DOWN" } else { "CENTER: up" },
                if right { "RIGHT : DOWN" } else { "RIGHT : up" },
            ],
        )
        .await;
        Timer::after(Duration::from_millis(100)).await;
    }
}
