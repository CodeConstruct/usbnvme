#![no_std]
#![forbid(unsafe_code)]

mod mctpusb;

pub use mctpusb::{MctpUsbClass, Sender, Receiver};

/// Maximum packet for DSP0283 1.0.
pub const MCTP_USB_MAX_PACKET: usize = 512;
