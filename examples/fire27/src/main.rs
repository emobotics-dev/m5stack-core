// SPDX-License-Identifier: BSD-3-Clause
//! M5Stack Fire27 (ESP32) BSP example — LVGL demo via oxivgl.
//!
//! Architecture note: flush_task must run on a higher-priority interrupt
//! executor so it can preempt the `waiti 0` spin in oxivgl's wait_callback.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![feature(type_alias_impl_trait)]

extern crate alloc;

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_executor::Spawner;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
esp_bootloader_esp_idf::esp_app_desc!();
use esp_backtrace as _;
use esp_hal::{
    ram,
    clock::CpuClock,
    gpio::{AnyPin, Level, Output, OutputConfig},
    interrupt::{Priority, software::SoftwareInterrupt, software::SoftwareInterruptControl},
    spi::master::{Config as SpiConfig, Spi},
    time::Rate,
    timer::timg::TimerGroup,
};
use esp_println as _;
use esp_rtos::embassy::InterruptExecutor;
use esp_sync::RawMutex;
use lcd_async::{
    Builder, Display,
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
};
use log::info;
use oxivgl::{
    lvgl_buffers::{flush_frame_buffer, DisplayOutput, LvglBuffers, UiError, COLOR_BUF_LINES},
    view::{run_lvgl, View},
    widgets::{Align, Bar, Label, Screen, WidgetError},
};
use static_cell::StaticCell;

// ── Screen geometry ──────────────────────────────────────────────────────────

const SCREEN_W: i32 = 320;
const SCREEN_H: i32 = 240;
const BUF_BYTES: usize = SCREEN_W as usize * COLOR_BUF_LINES * 2;

// ── Display driver ────────────────────────────────────────────────────────────

type SpiDev = SpiDeviceWithConfig<'static, RawMutex, Spi<'static, esp_hal::Async>, Output<'static>>;
type LcdDisplay = Display<SpiInterface<SpiDev, Output<'static>>, ILI9342CRgb565, Output<'static>>;

struct DisplayDriver {
    display: LcdDisplay,
}

// SAFETY: DisplayDriver is exclusively owned by flush_task; no concurrent access.
unsafe impl Send for DisplayDriver {}

impl DisplayOutput for DisplayDriver {
    async fn show_raw_data(&mut self, x: u16, y: u16, w: u16, h: u16, data: &[u8]) -> Result<(), UiError> {
        self.display.show_raw_data(x, y, w, h, data).await.map_err(|_| UiError::Display)
    }
}

// ── LVGL demo view ────────────────────────────────────────────────────────────

struct DemoView {
    _title: Label<'static>,
    _board: Label<'static>,
    _status: Label<'static>,
    _bar: Bar<'static>,
}

impl View for DemoView {
    fn create() -> Result<Self, WidgetError> {
        let screen = Screen::active().expect("lv_screen_active returned NULL");
        screen.bg_color(0x0a0a1a).bg_opa(255).remove_scrollable();

        let title = Label::new(&screen)?;
        title.text("oxivgl demo\0")?.align(Align::TopMid, 0, 12);
        title.text_color(0x00d0ff);

        let board = Label::new(&screen)?;
        board.text("M5Stack Fire27\0")?.align(Align::Center, 0, -20);
        board.text_color(0xffffff);

        let status = Label::new(&screen)?;
        status.text("LVGL ready\0")?.align(Align::Center, 0, 20);
        status.text_color(0x40ff80);

        let mut bar = Bar::new(&screen)?;
        bar.align(Align::BottomMid, 0, -24).size(240, 18);
        bar.set_range(100.0);
        bar.set_value(75.0);

        Ok(DemoView { _title: title, _board: board, _status: status, _bar: bar })
    }

    fn update(&mut self) -> Result<(), WidgetError> {
        Ok(())
    }
}

// ── Statics ───────────────────────────────────────────────────────────────────

static LVGL_BUFS: StaticCell<LvglBuffers<BUF_BYTES>> = StaticCell::new();
static SPI_BUS: StaticCell<Mutex<RawMutex, Spi<'static, esp_hal::Async>>> = StaticCell::new();
// Interrupt executor for flush_task (runs at Priority3, preempts waiti 0)
static INT_EXECUTOR: StaticCell<InterruptExecutor<1>> = StaticCell::new();

// ── Embassy tasks ─────────────────────────────────────────────────────────────

/// Runs on the interrupt executor — handles DMA/SPI flush for LVGL.
#[embassy_executor::task]
async fn flush_task(display: DisplayDriver) -> ! {
    flush_frame_buffer(display).await
}

/// Runs on the main executor — drives the LVGL render loop.
#[embassy_executor::task]
async fn ui_task(bufs: &'static mut LvglBuffers<BUF_BYTES>) -> ! {
    run_lvgl::<DemoView, BUF_BYTES>(SCREEN_W, SCREEN_H, bufs).await
}

// ── Halt handler ──────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    info!("custom_halt");
    loop {}
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 96 * 1024);

    let tg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    // SWI 0 → esp_rtos main executor. SWI 1 → our interrupt executor for flush_task.
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);

    // Steal SWI 1 for the interrupt executor (SWI 0 is already taken by esp_rtos).
    // SAFETY: SWI 1 is not used anywhere else.
    let swi1: SoftwareInterrupt<'static, 1> = unsafe { SoftwareInterrupt::steal() };
    let int_executor = INT_EXECUTOR.init(InterruptExecutor::new(swi1));
    let int_spawner = int_executor.start(Priority::Priority3);

    // ── SPI bus ───────────────────────────────────────────────────────────────
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_khz(400))
        .with_mode(esp_hal::spi::Mode::_0);
    let spi = Spi::new(peripherals.SPI2, spi_config.clone())
        .expect("SPI2 init failed")
        .with_sck(AnyPin::from(peripherals.GPIO18))
        .with_mosi(AnyPin::from(peripherals.GPIO23))
        .with_miso(AnyPin::from(peripherals.GPIO19))
        .into_async();

    let shared_spi = SPI_BUS.init(Mutex::new(spi));

    // ── Display ───────────────────────────────────────────────────────────────
    let display_cs = Output::new(AnyPin::from(peripherals.GPIO14), Level::High, OutputConfig::default());
    let mut bl = Output::new(AnyPin::from(peripherals.GPIO32), Level::Low, OutputConfig::default());
    let dc = Output::new(AnyPin::from(peripherals.GPIO27), Level::Low, OutputConfig::default());
    let rst = Output::new(AnyPin::from(peripherals.GPIO33), Level::Low, OutputConfig::default());

    let spi_device = SpiDeviceWithConfig::new(
        shared_spi,
        display_cs,
        spi_config.with_frequency(Rate::from_khz(40_000)),
    );
    let di = SpiInterface::new(spi_device, dc);
    let mut delay = embassy_time::Delay;
    let display = Builder::new(ILI9342CRgb565, di)
        .invert_colors(ColorInversion::Inverted)
        .color_order(ColorOrder::Bgr)
        .display_size(SCREEN_W as u16, SCREEN_H as u16)
        .reset_pin(rst)
        .init(&mut delay)
        .await
        .expect("Display init failed");

    bl.set_high();
    info!("Display initialized");

    let driver = DisplayDriver { display };
    let bufs = LVGL_BUFS.init(LvglBuffers::new());

    // flush_task → interrupt executor (Priority3), can preempt waiti 0 in wait_callback
    int_spawner.spawn(flush_task(driver)).expect("flush_task spawn failed");
    // ui_task → main executor (lower priority)
    spawner.spawn(ui_task(bufs)).expect("ui_task spawn failed");

    info!("Tasks spawned");
    loop {
        Timer::after(Duration::from_secs(10)).await;
    }
}
