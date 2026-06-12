// SPDX-License-Identifier: MIT OR Apache-2.0
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type, type_alias_impl_trait)]
//! LVGL example for the M5Stack Fire v2.7 (ESP32) and CoreS3 (ESP32-S3).
//!
//! Demonstrates driving the on-board ILI9342C display with the [`oxivgl`]
//! (LVGL) UI framework instead of hand-rolled drawing. The UI shows a title,
//! a continuously animating [`Spinner`], and a frame counter so that the
//! refresh/animation pipeline is visibly doing work.
//!
//! Dual-board: build for the Fire27 with the default `fire27` feature, or for
//! the CoreS3 with `--no-default-features --features cores3 --target
//! xtensa-esp32s3-none-elf`. The chip-agnostic oxivgl glue (`DisplayDriver`,
//! `flush_task`, `DemoView`, `run_app`) is shared; only the SPI/GPIO/PMIC
//! bring-up and the front-panel buttons differ per board.
//!
//! Hardware wiring (Fire27, ESP32):
//!
//! | Signal | GPIO |   | Signal     | GPIO |
//! |--------|------|---|------------|------|
//! | SCK    | 18   |   | DC         | 27   |
//! | MOSI   | 23   |   | RST        | 33   |
//! | MISO   | 19   |   | Backlight  | 32   |
//! | CS     | 14   |   |            |      |
//!
//! The three front-panel buttons (A=GPIO39 PREV, B=GPIO38 ENTER, C=GPIO37
//! NEXT, active-low with external pull-ups) are wired to an LVGL keypad input
//! device, mirroring oxivgl's `fire27` integration template.
//!
//! Hardware wiring (CoreS3, ESP32-S3): SPI2 SCK=GPIO36, MOSI=GPIO37, CS=GPIO3,
//! DC=GPIO35. There is no GPIO reset or backlight pin: the AW9523B expander
//! pulses the LCD/touch resets and the AXP2101 DLDO1 rail powers the backlight,
//! both over the shared I2C0 bus (SDA=GPIO12, SCL=GPIO11). The CoreS3 is touch
//! input only, so the front-panel button / keypad indev is fire27-only.
//!
//! The display flush runs on a high-priority [`InterruptExecutor`] (SWI1) so
//! the SPI transfer does not stall the LVGL render loop. The flush bus is an
//! explicit [`SpiDmaBus`]: on the ESP32 PDMA path a plain `Spi::into_async()`
//! flush goes "usr-stuck" after the first frame, so a descriptor-backed DMA
//! bus is required here (see the `SpiBusType` note below). The CoreS3 uses the
//! GDMA `DMA_CH0` channel for the same descriptor-backed bus.

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_executor::Spawner;
use embassy_sync::mutex::Mutex;
use embassy_time::Delay;
// Panic handler differs per board: Fire27 uses esp-backtrace (UART console);
// CoreS3 uses USB-Serial-JTAG, with which esp-backtrace/esp-println conflict, so
// it uses panic-halt + RTT instead (matching examples/cores3).
#[cfg(feature = "fire27")]
use esp_backtrace as _;
#[cfg(feature = "cores3")]
use panic_halt as _;
use esp_hal::{
    Async,
    clock::CpuClock,
    dma::{DmaRxBuf, DmaTxBuf},
    dma_buffers,
    gpio::{Level, Output, OutputConfig},
    interrupt::Priority,
    interrupt::software::SoftwareInterruptControl,
    ram,
    spi::{
        Mode,
        master::{Config as SpiConfig, Spi, SpiDmaBus},
    },
    time::Rate,
    timer::timg::TimerGroup,
};
#[cfg(feature = "fire27")]
use esp_println as _;
use esp_rtos::embassy::InterruptExecutor;
use esp_sync::RawMutex;
use lcd_async::{
    Builder, Display,
    interface::SpiInterface,
    models::ILI9342CRgb565,
    options::{ColorInversion, ColorOrder},
};
// CoreS3 resets the panel via the AW9523B expander, so its display takes the
// `NoResetPin` type parameter (no GPIO reset pin); Fire27 uses a GPIO reset.
#[cfg(feature = "cores3")]
use lcd_async::NoResetPin;
use log::info;
use oxivgl::{
    display::{COLOR_BUF_LINES, LvglBuffers},
    flush_pipeline::{DisplayOutput, UiError, flush_frame_buffer},
    style::{Selector, Style},
    view::{NavAction, View, run_app},
    widgets::{Align, Label, Obj, Spinner, WidgetError},
};
use static_cell::{StaticCell, make_static};

// Fire27-only imports: front-panel buttons + LVGL keypad indev.
#[cfg(feature = "fire27")]
use core::sync::atomic::{AtomicU32, Ordering};
#[cfg(feature = "fire27")]
use embassy_time::{Duration, Timer};
#[cfg(feature = "fire27")]
use esp_hal::gpio::{Input, InputConfig};
#[cfg(feature = "fire27")]
use oxivgl::enums::Key;

esp_bootloader_esp_idf::esp_app_desc!();

/// Halt quietly on panic so the backtrace is the only output (Fire27 only —
/// esp-backtrace's `custom-halt`; CoreS3 uses panic-halt).
#[cfg(feature = "fire27")]
#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    loop {}
}

const SCREEN_W: u16 = 320;
const SCREEN_H: u16 = 240;

/// LVGL render-buffer size in bytes: full width × `COLOR_BUF_LINES` lines ×
/// 2 bytes/pixel (RGB565). Two such buffers are double-buffered by LVGL.
const LVGL_BUF_BYTES: usize = SCREEN_W as usize * COLOR_BUF_LINES * 2;

// The flush bus is an explicit `SpiDmaBus` (not plain `Spi::into_async()`): on
// the ESP32 PDMA path of our esp-hal fork, the plain async-SPI flush gets
// "usr-stuck" after the first transfer (LVGL renders frame 1 then the flush
// hangs). A properly-configured DMA bus with descriptor-backed buffers avoids
// it — matching the `dma_display` example, which runs continuously on the fork.
// The CoreS3 (GDMA) shares the same descriptor-backed bus type.
type SpiBusType = SpiDmaBus<'static, Async>;
type SpiDeviceType = SpiDeviceWithConfig<'static, RawMutex, SpiBusType, Output<'static>>;
type DisplayInterface = SpiInterface<SpiDeviceType, Output<'static>>;
// The reset-pin type parameter differs per board: Fire27 drives a GPIO reset
// (`Output`), CoreS3 resets via the AW9523B expander (`NoResetPin`).
#[cfg(feature = "fire27")]
type LcdDisplay = Display<DisplayInterface, ILI9342CRgb565, Output<'static>>;
#[cfg(feature = "cores3")]
type LcdDisplay = Display<DisplayInterface, ILI9342CRgb565, NoResetPin>;

static SPI_BUS: StaticCell<Mutex<RawMutex, SpiBusType>> = StaticCell::new();

/// Glue between oxivgl's flush pipeline and the `lcd-async` display.
///
/// On the Fire27 it also owns the backlight pin (kept high for the lifetime of
/// the program); the CoreS3 has no GPIO backlight (the AXP2101 DLDO1 rail is
/// already on), so that field is fire27-only. The single [`DisplayOutput`]
/// method is what LVGL's flush task calls with each dirty rectangle.
struct DisplayDriver {
    #[cfg(feature = "fire27")]
    _bl: Output<'static>,
    display: LcdDisplay,
}

// SAFETY: `DisplayDriver` holds `Spi<Async>`, whose `PhantomData<*const ()>`
// makes it `!Send` to guard against accidental cross-thread sharing. On the
// single-core ESP32/ESP32-S3 the `flush_task` is the sole owner; no concurrent
// access occurs, so moving it onto the interrupt executor is sound.
unsafe impl Send for DisplayDriver {}

impl DisplayOutput for DisplayDriver {
    async fn show_raw_data(
        &mut self,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        data: &[u8],
    ) -> Result<(), UiError> {
        self.display
            .show_raw_data(x, y, w, h, data)
            .await
            .map_err(|_| UiError::Display)
    }
}

/// High-priority flush task: drains oxivgl's draw channel and pushes pixels
/// to the panel. Placed in RAM so it never stalls on flash access.
#[embassy_executor::task]
#[ram]
async fn flush_task(driver: DisplayDriver) -> ! {
    flush_frame_buffer(driver).await
}

// ---------------------------------------------------------------------------
// Hardware button → LVGL keypad input device (Fire27 only — verbatim from
// oxivgl's fire27 template). The CoreS3 is touch input only and omits this.
// ---------------------------------------------------------------------------

/// Pending LVGL key code written by the button tasks and consumed by the LVGL
/// read callback. `0` means "no pending key". Single-core ESP32: `Relaxed`
/// ordering is sufficient.
#[cfg(feature = "fire27")]
static KEY_PENDING: AtomicU32 = AtomicU32::new(0);

/// One task per button. Awaits a press edge, latches the LVGL key code, then
/// debounces the release so a single press maps to a single key event.
#[cfg(feature = "fire27")]
#[embassy_executor::task(pool_size = 3)]
async fn button_task(mut pin: Input<'static>, key_code: u32) -> ! {
    // Buttons are active-low (external pull-ups on GPIO34-39). Respond on the
    // PRESS edge for minimal latency — interrupt-driven, no polling.
    const DEBOUNCE: Duration = Duration::from_millis(15);
    loop {
        pin.wait_for_falling_edge().await;
        KEY_PENDING.store(key_code, Ordering::Relaxed);
        Timer::after(DEBOUNCE).await; // settle press bounce
        pin.wait_for_rising_edge().await; // wait for release
        Timer::after(DEBOUNCE).await; // settle release bounce
    }
}

/// LVGL keypad read callback, invoked by LVGL on every timer tick.
///
/// # Safety
/// Called from the LVGL task only (single-core ESP32). `data` is a non-null
/// pointer LVGL owns for the duration of this callback.
#[cfg(feature = "fire27")]
unsafe extern "C" fn keypad_read_cb(
    _indev: *mut oxivgl_sys::lv_indev_t,
    data: *mut oxivgl_sys::lv_indev_data_t,
) {
    let key = KEY_PENDING.swap(0, Ordering::Relaxed);
    // SAFETY: `data` is non-null and exclusively owned by LVGL here.
    unsafe {
        if key != 0 {
            (*data).key = key;
            (*data).state = oxivgl_sys::lv_indev_state_t_LV_INDEV_STATE_PRESSED;
        } else {
            (*data).state = oxivgl_sys::lv_indev_state_t_LV_INDEV_STATE_RELEASED;
        }
    }
}

/// Register the LVGL keypad input device backed by the three hardware buttons.
///
/// Must be called after `lv_init()` (i.e. from inside `View::create`, which
/// `run_app` invokes after driver init) and before any focusable widget needs
/// keypad routing.
#[cfg(feature = "fire27")]
fn register_keypad_indev() {
    // SAFETY: `lv_indev_create`/`set_type`/`set_read_cb` run after `lv_init()`
    // (guaranteed by `run_app` calling `create`). The indev pointer is checked
    // non-null and `keypad_read_cb` has the correct signature for a KEYPAD indev.
    unsafe {
        let indev = oxivgl_sys::lv_indev_create();
        assert!(!indev.is_null(), "lv_indev_create returned NULL");
        oxivgl_sys::lv_indev_set_type(indev, oxivgl_sys::lv_indev_type_t_LV_INDEV_TYPE_KEYPAD);
        oxivgl_sys::lv_indev_set_read_cb(indev, Some(keypad_read_cb));
    }
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// A small, lively demo screen: a title, a centered animated [`Spinner`], and
/// a frame counter that increments on every `update()` so refresh is visible.
///
/// Widgets are owned by the struct because LVGL deletes the underlying objects
/// when the wrapper is dropped — keeping them here keeps them alive.
#[derive(Default)]
struct DemoView {
    /// Frame counter label; its text is rewritten each `update()`.
    counter_label: Option<Label<'static>>,
    /// Kept alive for the program's lifetime (LVGL deletes on Drop).
    _title: Option<Label<'static>>,
    /// Kept alive for the program's lifetime (LVGL deletes on Drop).
    _spinner: Option<Spinner<'static>>,
    /// Frames rendered so far.
    frame: u32,
    /// `true` once the keypad indev has been registered (Fire27 only).
    #[cfg(feature = "fire27")]
    indev_registered: bool,
}

impl View for DemoView {
    fn create(&mut self, container: &Obj<'static>) -> Result<(), WidgetError> {
        // The CoreS3 is touch-only and registers no keypad indev.
        #[cfg(feature = "fire27")]
        if !self.indev_registered {
            register_keypad_indev();
            self.indev_registered = true;
        }

        let bg = Style::new(|s| {
            s.bg_color_hex(0x101820)
                .bg_opa(255)
                .text_color_hex(0xffffff);
        });
        container.add_style(&bg, Selector::DEFAULT);

        let title = Label::new(container)?;
        title
            // ASCII only: LVGL's built-in Montserrat font omits non-ASCII
            // glyphs (e.g. U+00B7 "·"), which render as a missing-glyph box.
            .text("m5stack-core - oxivgl")
            .align(Align::TopMid, 0, 12);

        let spinner = Spinner::new(container)?;
        spinner.size(90, 90).align(Align::Center, 0, -10);
        spinner.set_anim_params(1000, 200);

        let counter = Label::new(container)?;
        counter.text("frame: 0").align(Align::BottomMid, 0, -16);

        self._title = Some(title);
        self._spinner = Some(spinner);
        self.counter_label = Some(counter);
        Ok(())
    }

    fn update(&mut self) -> Result<NavAction, WidgetError> {
        self.frame = self.frame.wrapping_add(1);
        if let Some(label) = &self.counter_label {
            let mut buf = heapless::String::<24>::new();
            // Ignore formatting errors: the buffer is large enough for the
            // text, and a transient miss only skips one counter repaint.
            let _ = core::fmt::Write::write_fmt(&mut buf, format_args!("frame: {}", self.frame));
            label.text(&buf);
        }
        Ok(NavAction::None)
    }
}

#[esp_rtos::main]
async fn main(_low_prio_spawner: Spawner) {
    #[cfg(feature = "fire27")]
    esp_println::logger::init_logger_from_env();

    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // CoreS3 logs over RTT (read via `probe-rs run`/`attach`). Log at **Info**,
    // NOT rtt_init_log!()'s default of Trace: oxivgl emits a per-flush/per-tick
    // DEBUG stream, which floods the RTT buffer — and with no debugger draining
    // it, that back-pressures and STALLS the render loop (HIL-confirmed freeze).
    // At Info the demo only emits a few startup lines, so it runs standalone.
    // (esp-println over USB-Serial-JTAG is avoided entirely — it spin-waits on a
    // full FIFO.) Must run after `esp_hal::init` (RTT control-block setup).
    #[cfg(feature = "cores3")]
    rtt_target::rtt_init_log!(log::LevelFilter::Info);
    // LVGL allocates its object/style pool from the global heap. 50 KiB in the
    // reclaimed (post-boot ROM) DRAM region is ample for this UI; the
    // InterruptExecutor flush keeps DRAM-stack pressure low.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);

    let tg0 = TimerGroup::new(p.TIMG0);
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);
    info!("Embassy initialized");

    // SPI device config: 40 MHz, mode 0 — shared by both boards. The
    // SpiDeviceWithConfig re-applies this per transaction over the shared bus.
    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_khz(40_000))
        .with_mode(Mode::_0);

    // DMA buffers for the flush bus: rx is unused (write-only panel), tx holds
    // one LVGL render stripe. The SpiDmaBus copies user data through the tx
    // DmaTxBuf, so oxivgl's static draw buffers stay in plain DRAM.
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(64, LVGL_BUF_BYTES);
    let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("DMA rx buf alloc failed");
    let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).expect("DMA tx buf alloc failed");

    // -----------------------------------------------------------------------
    // Fire27 (ESP32): direct GPIO bring-up. SPI2 on the PDMA channel
    // (`DMA_SPI2`); DC=GPIO27, RST=GPIO33 (driven via the lcd-async reset pin),
    // BL=GPIO32. Three front-panel buttons → LVGL keypad indev.
    // -----------------------------------------------------------------------
    #[cfg(feature = "fire27")]
    let driver = {
        // Front-panel buttons. GPIO34-39 are input-only (no internal pull-up);
        // the Fire27 has external pull-ups, so the bare InputConfig is correct.
        let btn_a = Input::new(p.GPIO39, InputConfig::default()); // A — PREV
        let btn_b = Input::new(p.GPIO38, InputConfig::default()); // B — ENTER
        let btn_c = Input::new(p.GPIO37, InputConfig::default()); // C — NEXT

        // The task macro yields a `Result<SpawnToken, SpawnError>`; pool
        // exhaustion here is a startup logic bug (pool_size = 3 fits all three
        // buttons), so `.expect` is the right failure mode.
        _low_prio_spawner.spawn(button_task(btn_a, Key::PREV.0).expect("spawn button A"));
        _low_prio_spawner.spawn(button_task(btn_b, Key::ENTER.0).expect("spawn button B"));
        _low_prio_spawner.spawn(button_task(btn_c, Key::NEXT.0).expect("spawn button C"));

        let spi_bus = Spi::new(p.SPI2, spi_config.clone())
            .expect("SPI2 init failed")
            .with_sck(p.GPIO18)
            .with_mosi(p.GPIO23)
            .with_dma(p.DMA_SPI2)
            .with_buffers(dma_rx_buf, dma_tx_buf)
            .into_async();

        let shared_bus = SPI_BUS.init(Mutex::new(spi_bus));
        let display_cs = Output::new(p.GPIO14, Level::High, OutputConfig::default());
        let spi_device = SpiDeviceWithConfig::new(shared_bus, display_cs, spi_config);

        let mut bl = Output::new(p.GPIO32, Level::Low, OutputConfig::default());
        let dc = Output::new(p.GPIO27, Level::Low, OutputConfig::default());
        let rst = Output::new(p.GPIO33, Level::Low, OutputConfig::default());

        let di = SpiInterface::new(spi_device, dc);
        let mut delay = Delay;
        let display = Builder::new(ILI9342CRgb565, di)
            .invert_colors(ColorInversion::Inverted)
            .color_order(ColorOrder::Bgr)
            .display_size(SCREEN_W, SCREEN_H)
            .reset_pin(rst)
            .init(&mut delay)
            .await
            .expect("Display init failed");

        bl.set_high();
        info!("Display initialized, backlight on");

        DisplayDriver { _bl: bl, display }
    };

    // -----------------------------------------------------------------------
    // CoreS3 (ESP32-S3): the panel has no GPIO reset/backlight. The AW9523B
    // expander pulses the LCD/touch resets and the AXP2101 DLDO1 rail powers
    // the backlight, both over the shared I2C0 bus (SDA=GPIO12, SCL=GPIO11).
    // SPI2 runs on the GDMA `DMA_CH0` channel; DC=GPIO35 (must be a configured
    // Output so the pad routes), CS=GPIO3. No front-panel buttons (touch only).
    // Mirrors `examples/cores3/src/lib.rs::init_display`, inlined because this
    // example owns its own descriptor-backed `SpiDmaBus`.
    // -----------------------------------------------------------------------
    #[cfg(feature = "cores3")]
    let driver = {
        use esp_hal::i2c::master::{Config as I2cConfig, I2c};
        use m5stack_core::driver::aw9523b::{Aw9523bDriver, Aw9523bResources};
        use m5stack_core::driver::axp2101::Axp2101Driver;
        use m5stack_core::io::shared_i2c::SharedI2cBus;

        /// The AXP2101 PMIC I2C address (CoreS3 onboard).
        const AXP2101_ADDR: u8 = 0x34;

        // I2C0 @ 400 kHz on the CoreS3 bus pins, shared by the expander + PMIC.
        let i2c = I2c::new(
            p.I2C0,
            I2cConfig::default().with_frequency(Rate::from_khz(400)),
        )
        .expect("I2C0 init failed")
        .with_sda(p.GPIO12)
        .with_scl(p.GPIO11)
        .into_async();
        let i2c_bus: &'static SharedI2cBus = make_static!(SharedI2cBus::new(i2c));

        // AW9523B: pulse the LCD + touch resets (the CoreS3 has no GPIO reset).
        let mut aw = Aw9523bDriver::new(Aw9523bResources { i2c: i2c_bus });
        aw.init().await.expect("AW9523B init failed");
        aw.lcd_rst_pulse().await.expect("AW9523B LCD RST failed");
        aw.touch_rst_pulse()
            .await
            .expect("AW9523B TOUCH RST failed");

        // AXP2101: enable the DLDO1 rail (display backlight) and the battery ADC.
        let mut axp = Axp2101Driver::new(i2c_bus, AXP2101_ADDR);
        axp.set_dldo1(true, 3300)
            .await
            .expect("AXP2101 backlight enable failed");
        axp.enable_battery_adc()
            .await
            .expect("AXP2101 battery ADC enable failed");

        // SPI2 on the GDMA `DMA_CH0` channel (esp32s3 uses GDMA, not the ESP32's
        // PDMA `DMA_SPI2`).
        let spi_bus = Spi::new(p.SPI2, spi_config.clone())
            .expect("SPI2 init failed")
            .with_sck(p.GPIO36)
            .with_mosi(p.GPIO37)
            .with_dma(p.DMA_CH0)
            .with_buffers(dma_rx_buf, dma_tx_buf)
            .into_async();

        let shared_bus = SPI_BUS.init(Mutex::new(spi_bus));
        let display_cs = Output::new(p.GPIO3, Level::High, OutputConfig::default());
        let spi_device = SpiDeviceWithConfig::new(shared_bus, display_cs, spi_config);

        // DC on GPIO35: `Output::new` configures the pad's IO-MUX so the pin
        // actually drives — a bare GPIO-register hack leaves the pad unrouted
        // and DC never toggles, so the panel never wakes (black screen).
        let dc = Output::new(p.GPIO35, Level::Low, OutputConfig::default());

        let di = SpiInterface::new(spi_device, dc);
        let mut delay = Delay;
        // No `.reset_pin(...)`: reset was performed via the AW9523B above, so the
        // lcd-async builder takes the `NoResetPin` path (the cores3 `LcdDisplay`
        // alias above carries the matching `NoResetPin` type parameter).
        let display = Builder::new(ILI9342CRgb565, di)
            .invert_colors(ColorInversion::Inverted)
            .color_order(ColorOrder::Bgr)
            .display_size(SCREEN_W, SCREEN_H)
            .init(&mut delay)
            .await
            .expect("Display init failed");

        info!("Display initialized (CoreS3: AXP2101 backlight on)");

        DisplayDriver { display }
    };

    // Run the SPI flush on a high-priority interrupt executor (SWI1) so it
    // never blocks the LVGL render loop on the low-priority executor.
    let int_exec = make_static!(InterruptExecutor::new(sw_int.software_interrupt1));
    let hi_spawner = int_exec.start(Priority::min());
    hi_spawner.spawn(flush_task(driver).expect("spawn flush task"));

    static mut LVGL_BUFS: LvglBuffers<LVGL_BUF_BYTES> = LvglBuffers::new();
    // SAFETY: `LVGL_BUFS` is touched only here, before the single-threaded LVGL
    // render loop takes exclusive ownership of it for the rest of the program.
    let bufs = unsafe { &mut *core::ptr::addr_of_mut!(LVGL_BUFS) };

    run_app::<DemoView, LVGL_BUF_BYTES>(SCREEN_W.into(), SCREEN_H.into(), bufs, DemoView::default())
        .await
}
