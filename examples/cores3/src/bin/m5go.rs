// SPDX-License-Identifier: MIT OR Apache-2.0
//! CoreS3 — M5GO Battery Bottom: SK6812 LEDs + AXP2101 battery + M-Bus 5V rail.
//!
//! The bottom's LEDs sit on GPIO13 (M-Bus pin 23) — a *different* GPIO than the
//! Fire's pin-23/GPIO15. Unlike the Fire, the LED 5V rail is off by default and
//! must be enabled via the AW9523 expander; that enable is guarded against the
//! shared-VBUS contention case (see `enable_bus_5v` below).
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use cores3::{Lcd, STRIP_BYTES, draw_demo, draw_status, init_display, init_i2c, wheel};
use embassy_time::{Duration, Timer};
use esp_hal::{
    gpio::AnyPin, interrupt::software::SoftwareInterruptControl, ram, timer::timg::TimerGroup,
};
use m5stack_core::driver::aw9523b::{Aw9523bDriver, Aw9523bResources};
use m5stack_core::driver::sk6812::{Rgb, Sk6812Driver};
use panic_halt as _;
use rtt_target::rprintln;
use static_cell::make_static;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    // CRITICAL: esp_hal::init() MUST come before rtt_init_print!()
    let peripherals = esp_hal::init(esp_hal::Config::default());
    rtt_target::rtt_init_print!();
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);
    // NOTE: esp-alloc's global heap holds at most 3 regions — this internal heap,
    // the reclaimed region above, and the PSRAM region (below) are exactly 3, so
    // do NOT add a 4th `heap_allocator!`.
    esp_alloc::heap_allocator!(size: 64 * 1024);
    m5stack_core::mem::init_psram_heap(peripherals.PSRAM);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    let i2c_bus = init_i2c(
        peripherals.I2C0,
        AnyPin::from(peripherals.GPIO12),
        AnyPin::from(peripherals.GPIO11),
    );

    // `init_display` brings up the AW9523 (incl. its `init()`) and returns the
    // AXP2101 for battery reads; we re-open an AW9523 handle (it only holds the
    // shared bus) to enable the M-Bus 5V rail below.
    let shared_spi = make_static!(None);
    let (mut display, mut axp): (Lcd, _) = init_display(
        i2c_bus,
        shared_spi,
        peripherals.SPI2,
        AnyPin::from(peripherals.GPIO36),
        AnyPin::from(peripherals.GPIO37),
        AnyPin::from(peripherals.GPIO3),
        AnyPin::from(peripherals.GPIO35),
    )
    .await;
    let mut aw = Aw9523bDriver::new(Aw9523bResources { i2c: i2c_bus });

    let strip_buf: &'static mut [u8; STRIP_BYTES] = make_static!([0u8; STRIP_BYTES]);
    draw_demo(&mut display, &mut strip_buf[..], "CoreS3", &["M5GO bottom"]).await;

    // --- M5GO Battery Bottom: SK6812 LED bars on M-Bus pin 23 = GPIO13 on the
    // ESP32-S3 CoreS3 (a *different* GPIO than the Fire's pin-23/GPIO15). The
    // battery is read via the AXP2101 — CoreS3's own PMIC manages the cell, so
    // the bottom's IP5306 (used on the PMIC-less Basic Core / Fire) is not the
    // battery path here. Best-effort: LED writes go nowhere if absent.
    let mut leds = Sk6812Driver::new(peripherals.RMT, AnyPin::from(peripherals.GPIO13))
        .inspect_err(|e| rprintln!("SK6812 init failed: {:?}", e))
        .ok();
    let mut led_step: u8 = 0;

    // --- M5GO bottom 5V output: power the SK6812 LED bars ---
    // The bottom's LEDs are fed from the CoreS3 M-Bus 5V rail, which is the
    // SY7088 boost + load switch gated by the AW9523 (BOOST_EN=P1_7, BUS_OUT_EN
    // =P0_1, both active-HIGH — verified vs M5Unified). M5Unified only refuses to
    // enable it when there's NO battery AND USB is present (shared-VBUS contention),
    // so we replicate that guard: enable when a battery is present *or* USB is
    // absent. (The A014 bottom can't sustain CoreS3 on battery — it powers down on
    // unplug — so in practice this runs on USB with the bottom's battery present.)
    let vbus = axp.vbus_present().await.unwrap_or(true);
    let mv = axp.battery_voltage_mv().await.unwrap_or(0);
    let battery_present = mv > 3300;
    let bus_5v_on = if battery_present || !vbus {
        match aw.enable_bus_5v().await {
            Ok(()) => {
                rprintln!(
                    "M-Bus 5V enabled (BOOST_EN+BUS_OUT_EN); batt={}mV vbus={}",
                    mv,
                    vbus
                );
                true
            }
            Err(e) => {
                rprintln!("enable_bus_5v failed: {:?}", e);
                false
            }
        }
    } else {
        rprintln!("M-Bus 5V NOT enabled — no battery while on USB (contention guard)");
        false
    };
    {
        let l1 = alloc::format!("5V bus: {}", if bus_5v_on { "ON" } else { "OFF" });
        let l2 = alloc::format!("batt {}mV vbus={}", mv, if vbus { "Y" } else { "N" });
        draw_status(
            &mut display,
            &mut strip_buf[..],
            &[
                "M5GO LED test",
                "",
                &l1,
                &l2,
                "",
                "LEDs should cycle",
                "on G13",
            ],
        )
        .await;
    }
    Timer::after(Duration::from_millis(1500)).await;

    loop {
        let mut lines: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
        lines.push(alloc::string::String::from("M5GO LED + battery"));
        match (axp.battery_voltage_mv().await, axp.vbus_present().await) {
            (Ok(mv), Ok(vbus)) => lines.push(alloc::format!(
                "Batt {} mV {}",
                mv,
                if vbus { "USB" } else { "" }
            )),
            _ => lines.push(alloc::string::String::from("Batt: read err")),
        }
        lines.push(alloc::string::String::from(""));
        lines.push(alloc::format!(
            "5V bus: {}",
            if bus_5v_on { "ON" } else { "OFF" }
        ));
        lines.push(alloc::string::String::from("LEDs cycle on G13"));
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
                rprintln!("SK6812 write failed: {:?}", e);
            }
            led_step = led_step.wrapping_add(8);
        }

        Timer::after(Duration::from_millis(500)).await;
    }
}
