// SPDX-License-Identifier: MIT OR Apache-2.0
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type, type_alias_impl_trait)]
//! LVGL example for the M5Stack Fire v2.7 (ESP32).
//!
//! Demonstrates driving the on-board ILI9342C display with the [`oxivgl`]
//! (LVGL) UI framework instead of hand-rolled drawing. The UI shows a title,
//! a continuously animating [`Spinner`], and a frame counter so that the
//! refresh/animation pipeline is visibly doing work.
//!
//! Hardware wiring (Fire27):
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
//! The display flush runs on a high-priority [`InterruptExecutor`] (SWI1) so
//! the SPI transfer does not stall the LVGL render loop. The flush bus is an
//! explicit [`SpiDmaBus`]: on the ESP32 PDMA path a plain `Spi::into_async()`
//! flush goes "usr-stuck" after the first frame, so a descriptor-backed DMA
//! bus is required here (see the `SpiBusType` note below).

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDeviceWithConfig;
use embassy_executor::Spawner;
use embassy_sync::mutex::Mutex;
use embassy_time::{Delay, Duration, Timer};
use esp_backtrace as _;
use esp_hal::{
    Async,
    clock::CpuClock,
    dma::{DmaRxBuf, DmaTxBuf},
    dma_buffers,
    gpio::{Input, InputConfig, Level, Output, OutputConfig},
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
    display::{COLOR_BUF_LINES, LvglBuffers},
    enums::Key,
    flush_pipeline::{DisplayOutput, UiError, flush_frame_buffer},
    style::{Selector, Style},
    view::{NavAction, View, run_app},
    widgets::{Align, Label, Obj, Spinner, WidgetError},
};
use static_cell::{StaticCell, make_static};

esp_bootloader_esp_idf::esp_app_desc!();

/// Halt quietly on panic so the backtrace is the only output.
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
type SpiBusType = SpiDmaBus<'static, Async>;
type SpiDeviceType = SpiDeviceWithConfig<'static, RawMutex, SpiBusType, Output<'static>>;
type DisplayInterface = SpiInterface<SpiDeviceType, Output<'static>>;
type LcdDisplay = Display<DisplayInterface, ILI9342CRgb565, Output<'static>>;

static SPI_BUS: StaticCell<Mutex<RawMutex, SpiBusType>> = StaticCell::new();

/// Glue between oxivgl's flush pipeline and the `lcd-async` display.
///
/// Owns the backlight pin (kept high for the lifetime of the program) and the
/// initialized display, exposing the single [`DisplayOutput`] method LVGL's
/// flush task calls with each dirty rectangle.
struct DisplayDriver {
    _bl: Output<'static>,
    display: LcdDisplay,
}

// SAFETY: `DisplayDriver` holds `Spi<Async>`, whose `PhantomData<*const ()>`
// makes it `!Send` to guard against accidental cross-thread sharing. On the
// single-core ESP32 the `flush_task` is the sole owner; no concurrent access
// occurs, so moving it onto the interrupt executor is sound.
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
// Hardware button → LVGL keypad input device (verbatim from oxivgl's fire27)
// ---------------------------------------------------------------------------

/// Pending LVGL key code written by the button tasks and consumed by the LVGL
/// read callback. `0` means "no pending key". Single-core ESP32: `Relaxed`
/// ordering is sufficient.
static KEY_PENDING: AtomicU32 = AtomicU32::new(0);

/// One task per button. Awaits a press edge, latches the LVGL key code, then
/// debounces the release so a single press maps to a single key event.
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
    /// `true` once the keypad indev has been registered.
    indev_registered: bool,
}

impl View for DemoView {
    fn create(&mut self, container: &Obj<'static>) -> Result<(), WidgetError> {
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
async fn main(low_prio_spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // LVGL allocates its object/style pool from the global heap. 50 KiB in the
    // reclaimed (post-boot ROM) DRAM region is ample for this UI; the
    // InterruptExecutor flush keeps DRAM-stack pressure low.
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 50 * 1024);

    let tg0 = TimerGroup::new(p.TIMG0);
    let sw_int = SoftwareInterruptControl::new(p.SW_INTERRUPT);
    esp_rtos::start(tg0.timer0, sw_int.software_interrupt0);
    info!("Embassy initialized");

    // Front-panel buttons. GPIO34-39 are input-only (no internal pull-up);
    // the Fire27 has external pull-ups, so the bare InputConfig is correct.
    let btn_a = Input::new(p.GPIO39, InputConfig::default()); // A — PREV
    let btn_b = Input::new(p.GPIO38, InputConfig::default()); // B — ENTER
    let btn_c = Input::new(p.GPIO37, InputConfig::default()); // C — NEXT

    // The task macro yields a `Result<SpawnToken, SpawnError>`; pool exhaustion
    // here is a startup logic bug (pool_size = 3 fits all three buttons), so
    // `.expect` is the right failure mode.
    low_prio_spawner.spawn(button_task(btn_a, Key::PREV.0).expect("spawn button A"));
    low_prio_spawner.spawn(button_task(btn_b, Key::ENTER.0).expect("spawn button B"));
    low_prio_spawner.spawn(button_task(btn_c, Key::NEXT.0).expect("spawn button C"));

    let spi_config = SpiConfig::default()
        .with_frequency(Rate::from_khz(40_000))
        .with_mode(Mode::_0);

    // DMA buffers for the flush bus: rx is unused (write-only panel), tx holds
    // one LVGL render stripe. The SpiDmaBus copies user data through the tx
    // DmaTxBuf, so oxivgl's static draw buffers stay in plain DRAM.
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(64, LVGL_BUF_BYTES);
    let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("DMA rx buf alloc failed");
    let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).expect("DMA tx buf alloc failed");

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

    let driver = DisplayDriver { _bl: bl, display };

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
