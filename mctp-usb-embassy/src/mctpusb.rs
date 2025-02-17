#[cfg(feature = "defmt")]
#[allow(unused)]
use defmt::{debug, error, info, trace, warn};

#[cfg(feature = "log")]
#[allow(unused)]
use log::{debug, error, info, trace, warn};

use core::ops::Range;

use embassy_usb::descriptor::{SynchronizationType, UsageType};
use embassy_usb::Builder;
use embassy_usb_driver::{Driver, Endpoint, EndpointType, EndpointIn, EndpointOut};
use mctp_estack::usb::MctpUsbHandler;
use heapless::Vec;

use crate::MCTP_USB_MAX_PACKET;

pub const USB_CLASS_MCTP: u8 = 0x14;
// TODO naming?
pub const MCTP_SUBCLASS_DEVICE: u8 = 0x0;
pub const MCTP_PROTOCOL_V1: u8 = 0x1;

pub struct Sender<'d, D: Driver<'d>> {
    ep: D::EndpointIn,
    buf: Vec<u8, MCTP_USB_MAX_PACKET>,
}

impl<'d, D: Driver<'d>> Sender<'d, D> {
    /// Send a single packet.
    pub async fn send(&mut self, pkt: &[u8]) -> mctp::Result<()> {
        self.feed(pkt)?;
        self.flush().await
    }

    /// Enqueue a packet in the current USB payload.
    ///
    /// The payload will not be sent until `flush()` is called.
    /// May return [`mctp::Error::NoSpace`] if the packet won't
    /// fit in the current payload.
    pub fn feed(&mut self, pkt: &[u8]) -> mctp::Result<()> {
        let total = pkt.len().checked_add(4).ok_or(mctp::Error::NoSpace)?;
        let avail = self.buf.capacity() - self.buf.len();
        if avail < total {
            return Err(mctp::Error::NoSpace);
        }

        let mut hdr = [0u8; 4];
        MctpUsbHandler::header(pkt.len(), &mut hdr)?;
        let _ = self.buf.extend_from_slice(&hdr);
        let _ = self.buf.extend_from_slice(pkt);
        Ok(())
    }

    /// Send the current payload via USB.
    ///
    /// The payload must have been set with a previous `feed()`.
    pub async fn flush(&mut self) -> mctp::Result<()> {
        if self.buf.is_empty() {
            return Err(mctp::Error::BadArgument);
        }
        let r = self.ep.write(&self.buf).await;
        self.buf.clear();
        r.map_err(|_e| {
            mctp::Error::TxFailure
        })
    }

    pub async fn wait_connection(&mut self) {
        self.ep.wait_enabled().await
    }
}

pub struct Receiver<'d, D: Driver<'d>> {
    ep: D::EndpointOut,
    buf: [u8; MCTP_USB_MAX_PACKET],
    // valid range remaining in buf
    remaining: Range<usize>,
}

impl<'d, D: Driver<'d>> Receiver<'d, D> {
    /// Returns None on USB disconnected.
    pub async fn receive(&mut self) -> Option<mctp::Result<&[u8]>> {
        info!("receive");
        if self.remaining.is_empty() {
            trace!("empty");
            // Refill
            let l = match self.ep.read(&mut self.buf).await {
                Ok(l) => l,
                Err(_e) => {
                    warn!("recv failure");
                    return None
                }
            };
            trace!("refill l {}", l);
            self.remaining = Range { start: 0, end: l };
        }

        // TODO: would be nice to loop until a valid decode,
        // but lifetimes are difficult until polonius merges
        let rem = &self.buf[self.remaining.clone()];
        let (pkt, rem) = match MctpUsbHandler::decode(rem) {
            Ok(a) => a,
            Err(e) => {
                trace!("decode error");
                return Some(Err(e))
            }
        };
        trace!("rem len {}", rem.len());
        self.remaining.start = self.remaining.end - rem.len();
        Some(Ok(pkt))
    }

    pub async fn wait_connection(&mut self) {
        self.ep.wait_enabled().await
    }
}

pub struct MctpUsbClass<'d, D: Driver<'d>> {
    // TODO not pub
    pub sender: Sender<'d, D>,
    pub receiver: Receiver<'d, D>,
}

impl<'d, D: Driver<'d>> MctpUsbClass<'d, D> {
    pub fn new(builder: &mut Builder<'d, D>) -> Self {
        let mut func =
            builder.function(USB_CLASS_MCTP, MCTP_SUBCLASS_DEVICE, MCTP_PROTOCOL_V1);
        let mut iface = func.interface();
        // first alt iface is the default (and only)
        let mut alt = iface.alt_setting(
            USB_CLASS_MCTP,
            MCTP_SUBCLASS_DEVICE,
            MCTP_PROTOCOL_V1,
            None,
        );
        let interval = 1;
        let ep_out =
            alt.alloc_endpoint_out(EndpointType::Bulk, MCTP_USB_MAX_PACKET as u16, interval);
        let ep_in =
            alt.alloc_endpoint_in(EndpointType::Bulk, MCTP_USB_MAX_PACKET as u16, interval);

        alt.endpoint_descriptor(
            ep_out.info(),
            SynchronizationType::NoSynchronization,
            UsageType::DataEndpoint,
            &[],
        );
        alt.endpoint_descriptor(
            ep_in.info(),
            SynchronizationType::NoSynchronization,
            UsageType::DataEndpoint,
            &[],
        );

        let sender = Sender {
            ep: ep_in,
            buf: Vec::new(),
        };
        let receiver = Receiver {
            ep: ep_out,
            buf: [0; MCTP_USB_MAX_PACKET],
            remaining: Default::default(),
        };

        Self { sender, receiver }
    }

    pub fn split(self) -> (Sender<'d, D>, Receiver<'d, D>) {
        (self.sender, self.receiver)
    }
}
