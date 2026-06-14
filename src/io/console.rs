// SPDX-License-Identifier: MIT OR Apache-2.0
//! Async buffered serial console for both targets — byte-level ring buffer
//! with **overwrite-on-full** semantics. The producer (`log!()` / [`send_line`])
//! is O(1) and NEVER blocks the caller: never spins, never awaits, never
//! creates back-pressure. On full, the **oldest** bytes are overwritten so the
//! lines fired *just before* a failure survive (those are the informative ones).
//!
//! A single [`drain_task`] pulls contiguous slices from the ring and writes
//! them via the target's async TX sink (`write_all().await` parks on the
//! TX-done IRQ when the FIFO is full — no busy-spin). Producers signal the
//! drain after every write; the drain awaits the signal when the ring is empty.
//!
//! Per-target seam (hardware): the sink types + [`setup`] (build + split the
//! peripheral) + [`imp::boot_panic_write`]. fire27 = UART0 @ 1 Mbaud; cores3 =
//! USB-Serial-JTAG CDC. `setup` does NOT make the fire27 TX async — `into_async()`
//! binds the IRQ to the *calling* core, so the binary does it from `main` (PRO).
//!
//! The firmware's `alternator_regulator::logger::cat_line` calls [`send_line`]
//! for the `:cat` dump. Unlike `log!()`, [`send_line`] is **back-pressuring**:
//! it awaits ring space before writing, so a fast read-back self-paces to the TX
//! drain rate and is lossless (a plain overwrite-on-full write dropped lines and
//! made `log_interval`'s read-back show false gaps). alternator-regulator depends
//! on this crate ONLY for that — optional + esp-hal-gated, so host builds never
//! pull it.

use core::cell::RefCell;
use core::fmt::Write as _;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
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

    /// The peripheral bundle [`super::install`] needs to bring the console up:
    /// UART0 + its TX/RX pins (Fire27 console is UART0 @ 1 Mbaud).
    pub struct SerialResources {
        pub uart: UART0<'static>,
        pub tx_pin: AnyPin<'static>,
        pub rx_pin: AnyPin<'static>,
    }

    /// Build the console, returning the RX half (for `serial_cmd`) and the async
    /// TX sink the drain task writes. `into_async()` binds the TX-done IRQ to the
    /// calling core, so [`super::install`] must run on the core that owns the
    /// drain task (main / PRO).
    pub fn install_serial(
        res: SerialResources,
    ) -> (ConsoleRx<'static>, ConsoleTxAsync<'static>) {
        let (rx, tx) = setup(res.uart, res.tx_pin, res.rx_pin);
        (rx, tx.into_async())
    }

    // Raw UART0 TX-FIFO writer (panic only). Spins for FIFO room with a
    // bounded budget per byte: the panic message MUST get out — a dropped
    // [PANIC] line turns a clean panic into a silent "wedge" (cost a long
    // stack-overflow hunt on cores3). The bound keeps a dead/unclocked UART
    // from hanging the panic loop forever. Used by `on_panic` to synchronously
    // flush the ring after the async drain is gone (or never started). NEVER
    // call from steady-state code — `log!()` / `send_line` go through the ring.
    const UART0_FIFO_REG: *mut u32 = 0x3FF4_0000 as *mut u32;
    const UART0_STATUS_REG: *const u32 = 0x3FF4_001C as *const u32;
    const TX_FIFO_DEPTH: u32 = 128;
    /// ~a few ms at CPU speed — plenty for one byte at 1 Mbaud.
    const PANIC_SPIN_PER_BYTE: u32 = 1_000_000;

    pub fn boot_panic_write(bytes: &[u8]) {
        for &b in bytes {
            let mut budget = PANIC_SPIN_PER_BYTE;
            while unsafe { (UART0_STATUS_REG.read_volatile() >> 16) & 0xFF } >= TX_FIFO_DEPTH - 2 {
                budget -= 1;
                if budget == 0 {
                    return; // UART dead — give up rather than hang the panic
                }
                core::hint::spin_loop();
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

    /// The peripheral bundle [`super::install`] needs: just the USB-Serial-JTAG
    /// device (the CoreS3 console is the native CDC port — no probe needed, R7).
    pub struct SerialResources {
        pub usb: USB_DEVICE<'static>,
    }

    /// Build the console, returning the RX half (for `serial_cmd`) and the async
    /// TX sink the drain task writes. The split TX is already async on CoreS3.
    pub fn install_serial(
        res: SerialResources,
    ) -> (ConsoleRx<'static>, ConsoleTxAsync<'static>) {
        setup(res.usb)
    }

    // Raw SERIAL_JTAG EP1 FIFO writer (panic only). Spins for FIFO room with
    // a bounded budget per fill: the panic message MUST get out — the old
    // drop-on-full policy lost the [PANIC] line whenever the ring held more
    // than one 64-byte EP buffer of pre-panic context, turning every panic
    // into a silent "wedge" (cost a long stack-overflow hunt). The bound
    // keeps an unplugged USB host from hanging the panic loop forever.
    const SERIAL_JTAG_FIFO_REG: *mut u32 = 0x6003_8000 as *mut u32;
    const SERIAL_JTAG_CONF_REG: *mut u32 = 0x6003_8004 as *mut u32;
    /// ~tens of ms at CPU speed — the host polls the 64-byte EP every USB
    /// micro-frame, so a live host clears the FIFO well within this.
    const PANIC_SPIN_PER_FILL: u32 = 5_000_000;

    #[inline]
    fn fifo_full() -> bool {
        unsafe { SERIAL_JTAG_CONF_REG.read_volatile() & 0b010 == 0 }
    }

    pub fn boot_panic_write(bytes: &[u8]) {
        for &b in bytes {
            if fifo_full() {
                // Hand the queued bytes to the host, then wait (bounded) for
                // room. No host within the budget → give up, don't hang.
                unsafe { SERIAL_JTAG_CONF_REG.write_volatile(0b001) }; // flush (wr_done)
                let mut budget = PANIC_SPIN_PER_FILL;
                while fifo_full() {
                    budget -= 1;
                    if budget == 0 {
                        return;
                    }
                    core::hint::spin_loop();
                }
            }
            unsafe { SERIAL_JTAG_FIFO_REG.write_volatile(b as u32) };
        }
        unsafe { SERIAL_JTAG_CONF_REG.write_volatile(0b001) }; // flush (wr_done)
    }
}

pub use imp::{ConsoleRx, ConsoleTx, ConsoleTxAsync, SerialResources, setup};
use imp::{boot_panic_write, install_serial};

// ---- byte-level ring buffer (target-agnostic) ----

/// Ring capacity. ~50 lines × 80 B = ~4 KB — same memory budget as the prior
/// `Channel<Line, 12>` (~4.2 KB), and large enough to hold a message-only
/// panic plus the immediate pre-failure context. Bump if `esp-backtrace` is
/// ever wired up (a trace would add several KB).
const RING_SIZE: usize = 4096;
/// Per-line stack-format buffer. Largest record is the `[hil-cat]` CSV dump
/// (≈ 320 B + prefix + CRLF).
const LINE_CAP: usize = 352;

/// Byte ring with overwrite-on-full. Single struct held inside the mutex.
struct Ring {
    buf: [u8; RING_SIZE],
    head: usize, // write position (next byte to be written)
    tail: usize, // read position (next byte to be read)
    full: bool,  // disambiguates empty vs full when head == tail
    /// Bytes silently overwritten (oldest discarded) since the last drain read.
    /// The drain turns this into a VISIBLE `[CONSOLE-DROP …]` marker so a
    /// ring overrun never looks like a clean gap (which previously made
    /// `log_interval`'s read-back fail mysteriously). Saturates.
    dropped: u32,
}

impl Ring {
    const fn new() -> Self {
        Self { buf: [0; RING_SIZE], head: 0, tail: 0, full: false, dropped: 0 }
    }

    /// Append `bytes`; on full, the **oldest** bytes are overwritten. Always
    /// succeeds — no return value, no error path, no waiting. Called inside
    /// the mutex by both the log macros and `send_line`. Each overwritten byte
    /// bumps `dropped` so the drain can flag the loss.
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.buf[self.head] = b;
            self.head = (self.head + 1) % RING_SIZE;
            if self.full {
                // Overwrote the tail byte — advance tail to track it, and
                // record the drop so it surfaces downstream.
                self.tail = (self.tail + 1) % RING_SIZE;
                self.dropped = self.dropped.saturating_add(1);
            } else if self.head == self.tail {
                self.full = true;
            }
        }
    }

    /// Read + reset the overwritten-byte counter (drain-side).
    fn take_dropped(&mut self) -> u32 {
        core::mem::take(&mut self.dropped)
    }

    fn is_empty(&self) -> bool {
        !self.full && self.head == self.tail
    }

    /// Bytes currently queued (unread).
    fn used(&self) -> usize {
        if self.full {
            RING_SIZE
        } else if self.head >= self.tail {
            self.head - self.tail
        } else {
            RING_SIZE - self.tail + self.head
        }
    }

    /// Free bytes available before overwrite-on-full would discard data.
    fn free(&self) -> usize {
        RING_SIZE - self.used()
    }

    /// Copy up to `dst.len()` readable bytes into `dst` and advance `tail`.
    /// Bytes are taken from a single contiguous slice — if the ring wraps,
    /// the caller will get the rest on the next call. Returns bytes copied.
    fn read_and_consume(&mut self, dst: &mut [u8]) -> usize {
        if self.is_empty() {
            return 0;
        }
        // Length of the next contiguous readable slice.
        let n_contig = if self.head > self.tail {
            self.head - self.tail
        } else {
            RING_SIZE - self.tail
        };
        let n = n_contig.min(dst.len());
        let end = self.tail + n;
        dst[..n].copy_from_slice(&self.buf[self.tail..end]);
        self.tail = end % RING_SIZE;
        if n > 0 {
            self.full = false;
        }
        n
    }
}

/// The ring + a CriticalSectionRawMutex so any task on any core can write
/// safely. Lock scope is per-op (one memcpy + a few indices) — never held
/// across an `.await`, never spans more than one log line.
static RING: BlockingMutex<CriticalSectionRawMutex, RefCell<Ring>> =
    BlockingMutex::new(RefCell::new(Ring::new()));

/// Producer signal — woken after every ring write so the drain can resume
/// when the ring was empty. Idempotent (multiple signals before a wait =
/// one wait wakes); the drain reads everything it can per wakeup.
static DRAIN_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Drain → producer signal — raised after every drain read so a *back-pressuring*
/// producer ([`send_line`], the HIL `:cat` dump) can wake and retry once the ring
/// has freed space. The non-blocking hot log path ([`push_line`]) ignores it.
static SPACE_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Stack-format a line with a `[SSSSS.mmm LEVEL ] msg\r\n` header. Formats
/// WITHOUT the CRLF first, then guarantees termination so an over-long record is
/// truncated-but-terminated, never merged into the next (which would corrupt the
/// `[hil-cat]` CSV dump).
fn format_line(level: &str, args: core::fmt::Arguments<'_>) -> String<LINE_CAP> {
    let now = embassy_time::Instant::now();
    let mut line: String<LINE_CAP> = String::new();
    let _ = write!(
        line,
        "[{:05}.{:03} {:<5}] {}",
        now.as_secs(),
        now.as_millis() % 1000,
        level,
        args
    );
    while line.len() + 2 > LINE_CAP {
        let _ = line.pop();
    }
    let _ = line.push_str("\r\n");
    line
}

/// Format + push a line into the ring, signal the drain. O(1) on the caller
/// side: brief CS for the memcpy + an idempotent signal. **Non-blocking** —
/// on full the oldest bytes are overwritten (protects time-critical producers).
fn push_line(level: &str, args: core::fmt::Arguments<'_>) {
    let line = format_line(level, args);
    RING.lock(|r| r.borrow_mut().write(line.as_bytes()));
    DRAIN_SIGNAL.signal(());
}

struct ConsoleLogger;

impl log::Log for ConsoleLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        // Format level as a string so the column width matches the prior layout
        // exactly (existing log scrapers depend on it).
        let lvl = match record.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARN ",
            log::Level::Info => "INFO ",
            log::Level::Debug => "DEBUG",
            log::Level::Trace => "TRACE",
        };
        push_line(lvl, *record.args());
    }

    fn flush(&self) {}
}

static LOGGER: ConsoleLogger = ConsoleLogger;

/// Register the console as the global `log` backend. Call once, early. The
/// ring is statically initialised, so producers can write immediately —
/// pre-[`drain_task`] writes simply accumulate in the ring and are flushed
/// once the drain runs.
pub fn init() {
    init_with_level(log::LevelFilter::Info);
}

/// Like [`init`] but with an explicit max level (used by [`install`]).
fn init_with_level(level: log::LevelFilter) {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(level);
}

/// Stable, greppable log markers that the HIL `detect_crash` contract keys on.
/// Treat these as part of the public contract — do not reword without updating
/// the test harness.
pub mod markers {
    /// Emitted by [`super::on_panic`] before halt.
    pub const PANIC: &str = "[PANIC]";
    /// Prefix of the drain's ring-overrun marker (` <n>B]` follows).
    pub const CONSOLE_DROP: &str = "[CONSOLE-DROP";
    /// Logged once at boot when a previous-run panic breadcrumb is read back.
    pub const PREV_PANIC: &str = "[bc] previous panic";
}

/// Console install configuration. `serial: None` is the production backstop —
/// the log backend is registered (so `log!` is a cheap no-op into the ring) but
/// no transport is brought up and no drain task runs, leaving zero serial
/// surface. `Some(_)` brings up the chip's native transport (UART0 on Fire27,
/// USB-Serial-JTAG CDC on CoreS3) and spawns the drain.
pub struct Config {
    /// The chip's serial peripheral bundle, or `None` for a serial-free build.
    pub serial: Option<SerialResources>,
    /// Global `log` max level.
    pub level: log::LevelFilter,
}

/// What [`install`] hands back: the console RX half, when a transport was
/// brought up, to be handed to `serial_cmd` (bidirectional on one port, R7).
pub struct Console {
    /// `Some` iff `Config::serial` was `Some`. Hand to the serial-command reader.
    pub rx: Option<ConsoleRx<'static>>,
}

/// One-call console bring-up: register the `log` backend at `cfg.level` and,
/// when `cfg.serial` is `Some`, build the chip's transport + spawn the single
/// [`drain_task`]. Replaces the hand-wired `init()` + `setup()` + drain-spawn
/// sequence in every binary. Call once from main (the core that owns the drain;
/// Fire27's TX `into_async()` binds the IRQ to the calling core).
///
/// Returns a [`Console`] whose `rx` (when present) goes to the serial-command
/// reader so log TX and command RX share the one port (R7).
pub fn install(spawner: embassy_executor::Spawner, cfg: Config) -> Console {
    init_with_level(cfg.level);
    let rx = cfg.serial.map(|res| {
        let (rx, tx) = install_serial(res);
        crate::must_spawn!(spawner, drain_task(tx));
        rx
    });
    Console { rx }
}

/// Compatibility no-op. The prior design switched producers from a blocking
/// writer to an async queue here; with the ring buffer the producer path is
/// the same in every phase (boot and steady-state both write to the ring),
/// so there's nothing to switch. Kept so binaries don't have to change in
/// this commit — can be removed once both binaries stop calling it.
pub fn enable_async() {}

/// Bulk-dump emit (HIL `:cat` CSV read-back) — **back-pressuring** (lossless).
/// Unlike the hot log path ([`push_line`], which overwrites-oldest and never
/// blocks to protect time-critical producers like RWBLE), this AWAITS until the
/// ring has room for the whole line before writing, then signals the drain. So a
/// fast `:cat` dump self-paces to the TX drain rate instead of overflowing the
/// ~4 KB ring and dropping lines (which made `log_interval`'s read-back show
/// false gaps). Safe because the dump runs on a non-time-critical task and can
/// tolerate await latency; the `log!()` path does NOT use this.
pub async fn send_line(args: core::fmt::Arguments<'_>) {
    let line = format_line("INFO ", args);
    loop {
        // Reserve only if the WHOLE line fits — a partial write would still
        // overwrite-on-full and corrupt the dump. LINE_CAP < RING_SIZE, so it
        // always fits once the drain has caught up.
        let wrote = RING.lock(|r| {
            let mut r = r.borrow_mut();
            if r.free() >= line.len() {
                r.write(line.as_bytes());
                true
            } else {
                false
            }
        });
        if wrote {
            DRAIN_SIGNAL.signal(());
            return;
        }
        // Ring full — wait for the drain to free space, then retry. (No lost
        // wakeup: SPACE_SIGNAL latches, so a signal between the check above and
        // this wait still wakes us.)
        SPACE_SIGNAL.wait().await;
    }
}

/// The single console writer: pulls contiguous slices from the ring and
/// writes them via the target's async TX sink. `write_all().await` parks on
/// the TX-done IRQ when the FIFO is full — no spin, no interrupts-off, no
/// cross-core contention. When the ring is empty, awaits [`DRAIN_SIGNAL`]
/// (woken by every producer write). Spawn once from the binary's main
/// (fire27: pass `tx.into_async()`; cores3: the split TX is already async).
#[embassy_executor::task]
pub async fn drain_task(mut tx: ConsoleTxAsync<'static>) {
    // Per-iteration scratch. 256 B on the task stack is fine; the loop runs
    // again immediately to drain whatever didn't fit.
    let mut scratch = [0u8; 256];
    // Overflow-marker rate-limit state (see below).
    let mut drop_accum: u32 = 0;
    let mut last_drop_report = embassy_time::Instant::now();
    loop {
        // Read the next slice AND any overwrite count in one lock.
        let (n, dropped) = RING.lock(|r| {
            let mut r = r.borrow_mut();
            let n = r.read_and_consume(&mut scratch);
            (n, r.take_dropped())
        });
        drop_accum = drop_accum.saturating_add(dropped);
        // Surface a ring overrun as a LOUD, greppable marker so dropped bytes
        // never masquerade as a clean gap (the silent loss that made
        // `log_interval`'s read-back fail; host can grep `[CONSOLE-DROP`).
        //
        // RATE-LIMITED + coalesced: the marker is written by the drain straight
        // to TX (bypassing the ring, so it costs no ring space and can't itself
        // be overwritten), but it still shares TX bandwidth + drain time with
        // the data it's preserving. Emitting it every 256-B chunk under a
        // sustained log storm would steal drain throughput and AMPLIFY the
        // overrun (more drops → more markers → slower drain). So emit at most
        // ~4×/s with the accumulated byte count, plus an immediate flush the
        // moment the ring drains (episode end). Never blocks/back-pressures
        // producers — it only delays the drain slightly, now bounded.
        if drop_accum > 0 {
            let now = embassy_time::Instant::now();
            if n == 0 || (now - last_drop_report).as_millis() >= 250 {
                let mut mark: String<48> = String::new();
                let _ = write!(mark, "\r\n[CONSOLE-DROP {}B]\r\n", drop_accum);
                let _ = tx.write_all(mark.as_bytes()).await;
                drop_accum = 0;
                last_drop_report = now;
            }
        }
        if n == 0 {
            DRAIN_SIGNAL.wait().await;
            continue;
        }
        // Freed `n` bytes — wake any back-pressuring producer (the :cat dump).
        SPACE_SIGNAL.signal(());
        // Write outside the lock — `.await` is NEVER inside the ring CS.
        let _ = tx.write_all(&scratch[..n]).await;
    }
}

/// Shared message-only panic handler for both targets. Pushes the panic info
/// into the ring (alongside the pre-panic context that's already there), then
/// **synchronously drains the ring** via the raw FIFO poker — the async drain
/// task is gone (or never started) so it can't service the ring for us. After
/// the drain, halts so the fault stays visible. No stack walk: message-only
/// on both targets (deliberate, symmetric), which is why neither `esp-backtrace`
/// nor `esp-println` is pulled in. The binary's `#[panic_handler]` is a
/// one-line wrapper around this.
pub fn on_panic(info: &core::panic::PanicInfo<'_>) -> ! {
    // Push the panic into the ring. Producer-side is unchanged from any
    // normal log: brief CS, no await.
    let mut line: String<256> = String::new();
    let _ = write!(line, "\r\n[PANIC] {}\r\n", info);
    RING.lock(|r| r.borrow_mut().write(line.as_bytes()));

    // Synchronously drain everything in the ring via `boot_panic_write`.
    // Pull small chunks so the lock is brief and the raw write happens
    // outside the borrow (the write is itself sync, so the CS scope is still
    // per-chunk — but keeping them separate is cleaner).
    loop {
        let mut chunk = [0u8; 64];
        let n = RING.lock(|r| r.borrow_mut().read_and_consume(&mut chunk));
        if n == 0 {
            break;
        }
        boot_panic_write(&chunk[..n]);
    }

    loop {
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}
