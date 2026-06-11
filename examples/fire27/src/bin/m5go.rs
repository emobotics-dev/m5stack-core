// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fire27 — M5GO Battery Bottom: SK6812 LEDs + IP5306 battery.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock, gpio::AnyPin, interrupt::software::SoftwareInterruptControl, ram,
    timer::timg::TimerGroup,
};
use esp_println as _;
use fire27::{Lcd, STRIP_BYTES, draw_demo, draw_status, init_display, init_i2c, wheel};
use log::info;
use m5stack_core::driver::ip5306::{IP5306_ADDR, Ip5306Driver};
use m5stack_core::driver::sk6812::{Rgb, Sk6812Driver};
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
    // NOTE: esp-alloc's global heap holds at most 3 regions — these two plus the
    // PSRAM region (below) are exactly 3, so do NOT add a 4th `heap_allocator!`.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    esp_alloc::heap_allocator!(size: 64 * 1024);
    // The ESP32 cannot DMA out of PSRAM; registered here only to expose the heap.
    m5stack_core::mem::init_psram_heap(peripherals.PSRAM);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    let i2c_bus = init_i2c(
        peripherals.I2C0,
        AnyPin::from(peripherals.GPIO21),
        AnyPin::from(peripherals.GPIO22),
    );

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

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], "Fire27", &["M5GO bottom"]).await;

    // --- M5GO Battery Bottom: IP5306 fuel gauge (I2C 0x75, shared bus) and
    // SK6812 LED bars (M-Bus pin 23 = GPIO15 on the ESP32 Fire). Both are
    // best-effort: if the bottom isn't attached, the gauge reads as absent and
    // the LED writes go nowhere.
    let mut bottom_batt = Ip5306Driver::new(i2c_bus, IP5306_ADDR);
    let bottom_present = bottom_batt.present().await;
    info!("M5GO bottom IP5306 present: {}", bottom_present);

    let mut leds = Sk6812Driver::new(peripherals.RMT, AnyPin::from(peripherals.GPIO15))
        .inspect_err(|e| info!("SK6812 init failed: {:?}", e))
        .ok();
    let mut led_step: u8 = 0;

    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("M5GO LED + battery"));
        if bottom_present {
            match (
                bottom_batt.battery_level().await,
                bottom_batt.is_charging().await,
            ) {
                (Ok(pct), Ok(chg)) => lines.push(alloc::format!(
                    "Batt {}% {}",
                    pct,
                    if chg { "CHG" } else { "" }
                )),
                _ => lines.push(alloc::string::String::from("Batt: read err")),
            }
        } else {
            lines.push(alloc::string::String::from("IP5306 absent"));
        }
        lines.push(alloc::string::String::from(""));
        lines.push(alloc::string::String::from("LEDs cycle on G15"));
        let refs: alloc::vec::Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        draw_status(&mut display, &mut strip_buf[..], &refs).await;

        // Rotate a colour wheel across the 10 SK6812 LEDs on the bottom.
        if let Some(leds) = leds.as_mut() {
            let mut frame = [Rgb::OFF; 10];
            for (i, px) in frame.iter_mut().enumerate() {
                let (r, g, b) = wheel(led_step.wrapping_add((i as u8) * 25));
                // ~1/4 brightness — comfortable behind the frosted diffusers.
                *px = Rgb::new(r >> 2, g >> 2, b >> 2);
            }
            if let Err(e) = leds.write(&frame).await {
                info!("SK6812 write failed: {:?}", e);
            }
            led_step = led_step.wrapping_add(8);
        }

        Timer::after(Duration::from_millis(500)).await;
    }
}
