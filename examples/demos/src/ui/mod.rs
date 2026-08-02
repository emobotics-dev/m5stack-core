// SPDX-License-Identifier: MIT OR Apache-2.0
//! LVGL (oxivgl) demo UI — split into submodules so the `lvgl` bin's `main`
//! stays small. [`driver`] is the oxivgl flush glue over the BSP's DMA
//! display, [`view`] is the interactive demo screen, and [`input`] maps the
//! unified front-panel events into the LVGL keypad (both boards).

pub mod driver;
pub mod gauge;
pub mod input;
pub mod lvos;
pub mod lvprof;
pub mod metrics;
pub mod pipeline;
pub mod sched;
pub mod view;

use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use oxivgl::display::COLOR_BUF_LINES;

pub use driver::{DisplayDriver, flush_task};
pub use view::MenuView;

pub use m5stack_core::board::display::{SCREEN_H, SCREEN_W};

/// LVGL render-buffer size in bytes: full width × `COLOR_BUF_LINES` lines ×
/// 2 bytes/pixel (RGB565). Two such buffers are double-buffered by LVGL.
pub const LVGL_BUF_BYTES: usize = SCREEN_W as usize * COLOR_BUF_LINES * 2;

/// Allocate the flush bus's DMA buffers: RX unused (write-only panel), TX holds
/// one LVGL render stripe.
pub fn dma_bufs() -> (DmaRxBuf, DmaTxBuf) {
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(64, LVGL_BUF_BYTES);
    let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("DMA rx buf alloc failed");
    let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).expect("DMA tx buf alloc failed");
    (dma_rx_buf, dma_tx_buf)
}
