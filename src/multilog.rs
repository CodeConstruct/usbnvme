// SPDX-License-Identifier: GPL-3.0-only
/*
 * Copyright (c) 2025 Code Construct
 */
#![allow(clippy::collapsible_if)]
use core::cell::Cell;
use core::fmt::Write;

use log::{Log, Metadata, Record};
use rtt_target::{rprintln, rtt_init_print};

pub use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
pub use embassy_sync::channel::Channel;

use heapless::String;

use crate::now;

// Aribtrary limits, limited by RAM
const MAX_LINE: usize = 120;
pub const SERIAL_BACKLOG: usize = 50;

pub type RawMutex = embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
type Line = String<MAX_LINE>;

static LOGGER: MultiLog = MultiLog::new();

#[allow(dead_code)]
type UsbSerialSender = embassy_usb::class::cdc_acm::Sender<
    'static,
    embassy_stm32::usb::Driver<'static, embassy_stm32::peripherals::USB_OTG_HS>,
>;

pub fn init() {
    LOGGER.start();
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
}

#[embassy_executor::task]
pub async fn log_usbserial_task(mut sender: UsbSerialSender) {
    /// Writes a buffer in cdc sized chunks
    async fn write_cdc(
        sender: &mut UsbSerialSender,
        b: &[u8],
    ) -> Result<(), ()> {
        for pkt in b.chunks(64) {
            if let Err(e) = sender.write_packet(pkt).await {
                rprintln!("usbserial err {:?}", e);
                return Err(());
            }
        }
        // cdc acm zero length packet
        if b.len() % 64 == 0 {
            sender.write_packet(&[]).await.map_err(|_| ())?;
        }
        Ok(())
    }

    // Outer loop for reattaching USB
    loop {
        sender.wait_connection().await;
        // inner loop writing log lines while connected
        'connected: loop {
            let s = LOGGER.serial_backlog.receive().await;
            if write_cdc(&mut sender, s.as_bytes()).await.is_err() {
                break 'connected;
            }
            if !s.ends_with("\r") {
                if write_cdc(&mut sender, b" (line truncated)\r")
                    .await
                    .is_err()
                {
                    break 'connected;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LostLine {
    No,
    Lost,
    Warned,
}

struct MultiLog {
    serial_backlog: Channel<RawMutex, Line, SERIAL_BACKLOG>,
    serial_lost_lines: BlockingMutex<RawMutex, Cell<LostLine>>,
}

impl MultiLog {
    const fn new() -> Self {
        Self {
            serial_backlog: Channel::new(),
            serial_lost_lines: BlockingMutex::new(Cell::new(LostLine::No)),
        }
    }

    fn start(&self) {
        // RTT default is non-blocking (drop on full), 1024 byte buffer
        rtt_init_print!();
    }

    fn log_usbserial(&self, record: &Record, msg: Line) {
        if record.level() > log::Level::Info {
            // Avoid filling queue with debug or trace logs
            return;
        }

        self.serial_lost_lines.lock(|lost| {
            // Warn once for each span of lost log messages (backlog full)
            if lost.get() == LostLine::Lost {
                let l = "(missed log)\r".try_into().unwrap();
                if self.serial_backlog.try_send(l).is_err() {
                    return;
                }
                lost.set(LostLine::Warned);
            }

            // Try to enqueue the log message
            match self.serial_backlog.try_send(msg) {
                Ok(_) => lost.set(LostLine::No),
                Err(_) => {
                    if lost.get() == LostLine::No {
                        lost.set(LostLine::Lost);
                    }
                }
            }
        });
    }
}

impl Log for MultiLog {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        // TODO filtering
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let now = now();
        rprintln!("{:10} {:<5} {}", now, record.level(), record.args());

        let mut s = Line::new();
        // Truncated writes will be reported by the other end, detecting \r
        let _ = write!(
            &mut s,
            "{:10} {:<5} {} \r",
            now,
            record.level(),
            record.args()
        );
        self.log_usbserial(record, s);
    }

    fn flush(&self) {}
}
