// SPDX-License-Identifier: MIT OR Apache-2.0
//! Async buffered serial console for both targets — the COMPLETE console: the
//! target-agnostic pipeline (a `log::Log` backend + a cross-core queue + the
//! drain task) AND the per-target hardware (sink construction + the raw boot/
//! panic FIFO writer). No esp-println, no esp-backtrace.
//!
//! A single `log::Log` backend formats each record and either (steady state)
//! `try_send`s it to the cross-core channel drained by [`drain_task`] on the
//! target's async TX sink, or (boot + panic) writes it via the raw per-target
//! `imp::boot_panic_write`. Producers never block on the sink and the single drain
//! task is the sole writer — so there's no cross-core print-lock contention to
//! starve the radio (the recurring-freeze root cause).
//!
//! Per-target seam (hardware): the sink types + [`setup`] (build + split the
//! peripheral) + the raw FIFO writer. fire27 = UART0 @ 1 Mbaud; cores3 =
//! USB-Serial-JTAG CDC. `setup` does NOT make the fire27 TX async — `into_async()`
//! binds the IRQ to the *calling* core, so the binary does it from `main` (PRO).
//!
//! The firmware's `alternator_regulator::logger::cat_line` calls [`send_line`]
//! for the `:cat` dump (back-pressure). alternator-regulator depends on this
//! crate ONLY for that — optional + esp-hal-gated, so host builds never pull it.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embedded_io_async::Write as _;
use heapless::String;

#[cfg(feature = "fire27")]
mod imp {
    use esp_hal::{
        Async, Blocking,
        gpio::AnyPin,
        peripherals::UART0,
        uart::{Config, Uart, UartRx, UartTx},
    };

    /// RX half → `serial_cmd`; TX (blocking) → made async by the binary at
    /// drain-spawn (`into_async` binds the IRQ to the calling = PRO core).
    pub type ConsoleRx<'d> = UartRx<'d, Blocking>;
    pub type ConsoleTx<'d> = UartTx<'d, Blocking>;
    /// The drain task's sink — the TX after the binary's `into_async()`.
    pub type ConsoleTxAsync<'d> = UartTx<'d, Async>;

    /// Build UART0 @ 1 Mbaud and split. Run early (before radio bring-up); HIL-
    /// confirmed safe with the async console (see memory fire27-uart-async-corrected).
    pub fn setup(
        uart: UART0<'static>,
        tx_pin: AnyPin<'static>,
        rx_pin: AnyPin<'static>,
    ) -> (ConsoleRx<'static>, ConsoleTx<'static>) {
        Uart::new(uart, Config::default().with_baudrate(1_000_000))
            .expect("UART0 console init")
            .with_tx(tx_pin)
            .with_rx(rx_pin)
            .split()
    }

    // Raw UART0 TX-FIFO writer (boot + panic). UART0_FIFO_REG = base; bits
    // [23:16] of UART0_STATUS_REG = TX FIFO byte count (depth 128). Write only
    // while the FIFO has space, bounded; drop the rest on budget exhaustion so
    // the cross-core print path never holds interrupts off for a wire-drain and
    // starves RWBLE (the recurring-freeze root cause — see fire27-recurring-freeze).
    const UART0_FIFO_REG: *mut u32 = 0x3FF4_0000 as *mut u32;
    const UART0_STATUS_REG: *const u32 = 0x3FF4_001C as *const u32;
    const TX_FIFO_DEPTH: u32 = 128;
    const SPIN_BUDGET: u32 = 4_000;

    pub fn boot_panic_write(bytes: &[u8]) {
        let mut budget = SPIN_BUDGET;
        for &b in bytes {
            while unsafe { (UART0_STATUS_REG.read_volatile() >> 16) & 0xFF } >= TX_FIFO_DEPTH - 2 {
                if budget == 0 {
                    return;
                }
                budget -= 1;
            }
            unsafe { UART0_FIFO_REG.write_volatile(b as u32) };
        }
    }
}

#[cfg(feature = "cores3")]
mod imp {
    use esp_hal::{
        Async,
        peripherals::USB_DEVICE,
        usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx, UsbSerialJtagTx},
    };

    /// RX half → `serial_cmd` (async poller); TX half → the drain task.
    pub type ConsoleRx<'d> = UsbSerialJtagRx<'d, Async>;
    pub type ConsoleTx<'d> = UsbSerialJtagTx<'d, Async>;
    /// The drain task's sink — the split TX is already async on cores3.
    pub type ConsoleTxAsync<'d> = UsbSerialJtagTx<'d, Async>;

    /// Build the USB-Serial-JTAG console and split. `into_async()` here binds the
    /// IRQ to whatever core calls this — the binary calls it from main.
    pub fn setup(usb: USB_DEVICE<'static>) -> (ConsoleRx<'static>, ConsoleTx<'static>) {
        UsbSerialJtag::new(usb).into_async().split()
    }

    // Raw SERIAL_JTAG EP1 FIFO writer (boot + panic). CONF bit1 = data-free
    // (clear ⇒ full); FIFO reg takes one byte; CONF bit0 = wr_done (flush).
    // Bounded spin per byte, drop remainder on a stuck (host-less) FIFO.
    const SERIAL_JTAG_FIFO_REG: *mut u32 = 0x6003_8000 as *mut u32;
    const SERIAL_JTAG_CONF_REG: *mut u32 = 0x6003_8004 as *mut u32;
    const SPIN_BUDGET: u32 = 50_000;

    #[inline]
    fn fifo_full() -> bool {
        unsafe { SERIAL_JTAG_CONF_REG.read_volatile() & 0b010 == 0 }
    }

    pub fn boot_panic_write(bytes: &[u8]) {
        for &b in bytes {
            let mut budget = SPIN_BUDGET;
            while fifo_full() {
                if budget == 0 {
                    return;
                }
                budget -= 1;
            }
            unsafe { SERIAL_JTAG_FIFO_REG.write_volatile(b as u32) };
        }
        unsafe { SERIAL_JTAG_CONF_REG.write_volatile(0b001) }; // flush (wr_done)
    }
}

pub use imp::{ConsoleRx, ConsoleTx, ConsoleTxAsync, setup};
use imp::boot_panic_write;

// ---- target-agnostic pipeline ----

/// Per-line buffer. Must fit the largest record: the `[hil-cat]` CSV dump emits
/// lines up to 320 B + the prefix + CRLF. Too small truncates and loses the
/// newline, merging records (corrupts the dump).
const LINE_CAP: usize = 352;
/// Queue depth — buffers bursts (the `[host]` init, a `[hil-cat]` dump) so they
/// pace out, not FIFO-drop. SD-I/O-paced producers let the drain keep up.
const QUEUE_DEPTH: usize = 12;
type Line = String<LINE_CAP>;

static QUEUE: Channel<CriticalSectionRawMutex, Line, QUEUE_DEPTH> = Channel::new();
static ASYNC_MODE: AtomicBool = AtomicBool::new(false);

struct ConsoleLogger;

impl log::Log for ConsoleLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let now = embassy_time::Instant::now();
        let mut line: Line = String::new();
        // "[SSSSS.mmm LEVEL ] msg". Format WITHOUT the CRLF, then guarantee
        // termination so an over-long record is truncated-but-terminated, never
        // merged into the next (which corrupts e.g. the [hil-cat] CSV dump).
        let _ = write!(
            line,
            "[{:05}.{:03} {:<5}] {}",
            now.as_secs(),
            now.as_millis() % 1000,
            record.level(),
            record.args()
        );
        while line.len() + 2 > LINE_CAP {
            let _ = line.pop();
        }
        let _ = line.push_str("\r\n");
        if ASYNC_MODE.load(Ordering::Relaxed) {
            // Non-blocking: drop on a full queue rather than block (never wedges
            // the radio). Bulk dumps use back-pressuring `send_line` instead.
            let _ = QUEUE.try_send(line);
        } else {
            boot_panic_write(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

static LOGGER: ConsoleLogger = ConsoleLogger;

/// Register the console as the global `log` backend. Call once, early, before
/// the first log line. Starts in BLOCKING mode (the raw FIFO writer) until
/// [`enable_async`].
pub fn init() {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}

/// Switch producers from the blocking writer to the async channel. Call after
/// the [`drain_task`] has been spawned.
pub fn enable_async() {
    ASYNC_MODE.store(true, Ordering::Relaxed);
}

/// Back-pressuring emit for BULK dumps (the `[hil-cat]` CSV read-back): formats
/// the line and AWAITS channel space instead of dropping, so a fast dump self-
/// paces to the drain rate and can't overflow at 1 Mbaud. Normal log records
/// stay non-blocking (drop-on-full) via `log::Log`, so logging never wedges the
/// radio. Call from a task whose executor also runs [`drain_task`]; before async
/// mode (boot), falls back to the blocking writer.
pub async fn send_line(args: core::fmt::Arguments<'_>) {
    let now = embassy_time::Instant::now();
    let mut line: Line = String::new();
    let _ = write!(
        line,
        "[{:05}.{:03} INFO ] {}",
        now.as_secs(),
        now.as_millis() % 1000,
        args
    );
    while line.len() + 2 > LINE_CAP {
        let _ = line.pop();
    }
    let _ = line.push_str("\r\n");
    if ASYNC_MODE.load(Ordering::Relaxed) {
        QUEUE.send(line).await; // back-pressure: yields until the drain frees a slot
    } else {
        boot_panic_write(line.as_bytes());
    }
}

/// The single console writer: drains the queue, writing each line interrupt-
/// driven via the target's async TX sink (it sleeps during the FIFO drain — no
/// interrupts-off, no cross-core contention). Spawn once from the binary's main
/// (fire27: pass `tx.into_async()`; cores3: the split TX is already async).
#[embassy_executor::task]
pub async fn drain_task(mut tx: ConsoleTxAsync<'static>) {
    loop {
        let line = QUEUE.receive().await;
        let _ = tx.write_all(line.as_bytes()).await;
    }
}

/// Shared message-only panic handler for both targets. Prints the panic info via
/// the raw blocking writer (NOT the async queue — the drain task is gone by now),
/// then halts so the fault stays visible (no silent reset masking it as a
/// reboot). No stack walk: message-only on both targets (deliberate, symmetric),
/// which is why neither esp-backtrace nor esp-println is pulled in. The binary's
/// `#[panic_handler]` is a one-line wrapper around this.
pub fn on_panic(info: &core::panic::PanicInfo<'_>) -> ! {
    let mut line: String<256> = String::new();
    let _ = write!(line, "\r\n[PANIC] {}\r\n", info);
    boot_panic_write(line.as_bytes());
    loop {
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}
