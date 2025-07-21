//! Handlers for Code Construct testing protocols.
//!
//! `mctp-echo` and `mctp-bench`

// SPDX-License-Identifier: GPL-3.0-only
/*
 * Copyright (c) 2025 Code Construct
 */
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use core::num::Wrapping;

use num_derive::FromPrimitive;
use num_traits::FromPrimitive;

use deku::prelude::*;
use mctp::{
    AsyncListener, AsyncReqChannel, AsyncRespChannel, Eid, Error, Result,
};

use crate::SignalCS;

pub struct MctpBench<'a> {
    buf: &'a mut [u8],
}

impl<'a> MctpBench<'a> {
    const VENDOR_SUBTYPE: [u8; 3] = [0xcc, 0xde, 0xf1];
    const MAGIC: u16 = 0xbeca;
    const SEQ_START: u32 = u32::MAX - 5;

    const COMMAND_MAGIC: u16 = 0x22dd;
    const COMMAND_VERSION: u8 = 1;

    const BENCH_HEADER_LEN: usize = 9;

    pub fn new(buf: &'a mut [u8]) -> Result<Self> {
        if buf.len() < Self::BENCH_HEADER_LEN {
            return Err(Error::BadArgument);
        }
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i & 0xff) as u8;
        }
        buf[..3].copy_from_slice(&Self::VENDOR_SUBTYPE);
        buf[3..5].copy_from_slice(&Self::MAGIC.to_le_bytes());

        Ok(Self { buf })
    }

    pub async fn send(
        &mut self,
        req: &mut impl AsyncReqChannel,
        count: u64,
        len: usize,
    ) -> Result<()> {
        if len < 9 {
            return Err(Error::BadArgument);
        }
        let buf = self.buf.get_mut(..len).ok_or(Error::BadArgument)?;

        let mut counter = Wrapping(Self::SEQ_START);
        for _ in 0..count {
            buf[5..9].copy_from_slice(&counter.0.to_le_bytes());
            counter += 1;

            req.send(mctp::MCTP_TYPE_VENDOR_PCIE, buf).await?;
        }
        Ok(())
    }

    pub async fn handle_request(
        msg: &[u8],
        resp: &mut impl AsyncRespChannel,
        bench_request: &SignalCS<BenchRequest>,
    ) -> Result<()> {
        let Ok(((rest, _), cmd)) = MctpBenchCommandMsg::from_bytes((msg, 0))
        else {
            trace!("Short bench command");
            return Err(Error::InvalidInput);
        };

        if cmd.vendor_prefix != Self::VENDOR_SUBTYPE
            || cmd.magic != Self::COMMAND_MAGIC
            || cmd.version != Self::COMMAND_VERSION
        {
            trace!("Bad command {cmd:?}");
            return Err(Error::InvalidInput);
        }

        let req_cmd = CommandCode::from_u8(cmd.command);

        let resp_code = if let Some(req_cmd) = req_cmd {
            match Self::handle_command(
                req_cmd,
                rest,
                bench_request,
                resp.remote_eid(),
            )
            .await
            {
                Ok(()) => CommandResponse::Success,
                Err(e) => e,
            }
        } else {
            CommandResponse::UnknownCommand
        };

        // Response has mostly the same parameters as the request cmd
        let r = MctpBenchCommandMsg {
            command: CommandCode::Response as u8,
            ..cmd
        };

        let mut buf = [0u8; 13];
        let l = r.to_slice(&mut buf).unwrap();
        // body is a single status byte
        buf[l] = resp_code as u8;
        let buf = &buf[..l + 1];

        resp.send(buf).await
    }

    async fn handle_command(
        cmd: CommandCode,
        body: &[u8],
        bench_request: &SignalCS<BenchRequest>,
        peer: Eid,
    ) -> core::result::Result<(), CommandResponse> {
        match cmd {
            CommandCode::RequestBench => {
                let Ok(((rest, _), req)) =
                    CommandRequestBench::from_bytes((body, 0))
                else {
                    trace!("Short bench request");
                    return Err(CommandResponse::Error);
                };
                if !rest.is_empty() {
                    trace!("Long bench request");
                    return Err(CommandResponse::Error);
                }

                if (req.payload_size as usize) < Self::BENCH_HEADER_LEN {
                    trace!("Requested payload too short");
                    return Err(CommandResponse::BadArgument);
                }

                bench_request.signal(BenchRequest {
                    count: req.message_count,
                    len: req.payload_size as usize,
                    dest: peer,
                })
            }
            CommandCode::Response => {
                trace!("Response as request");
                return Err(CommandResponse::Error);
            }
        }
        Ok(())
    }
}

#[repr(u8)]
#[derive(FromPrimitive, Debug)]
enum CommandCode {
    Response = 0x00,
    RequestBench = 0x01,
}

#[repr(u8)]
#[derive(FromPrimitive, Debug)]
enum CommandResponse {
    Success = 0x00,
    Error = 0x01,
    UnknownCommand = 0x02,
    BadArgument = 0x03,
}

// Matches mctp-bench.c struct command_msg
#[derive(DekuRead, DekuWrite, Debug, Clone)]
#[deku(endian = "little")]
struct MctpBenchCommandMsg {
    vendor_prefix: [u8; 3],
    magic: u16,

    version: u8,
    command: u8,
    iid: u32,
    // followed by command-specific body
}

// mctp-bench.c struct command_request_bench
#[derive(DekuRead, DekuWrite, Debug)]
#[deku(endian = "little")]
struct CommandRequestBench {
    flags: u32,
    payload_size: u16,
    message_count: u64,
}

/// Notification of a bench request
#[derive(Debug, Clone)]
pub struct BenchRequest {
    pub count: u64,
    pub len: usize,
    pub dest: Eid,
}

pub async fn listener(
    router: &'static mctp_estack::Router<'static>,
    bench_request: &SignalCS<BenchRequest>,
) -> ! {
    const VENDOR_SUBTYPE_ECHO: [u8; 3] = [0xcc, 0xde, 0xf0];

    let mut l = router.listener(mctp::MCTP_TYPE_VENDOR_PCIE).unwrap();
    let mut buf = [0u8; 100];
    loop {
        let Ok((_typ, _ic, msg, mut resp)) = l.recv(&mut buf).await else {
            warn!("echo Bad listener recv");
            continue;
        };

        if msg.starts_with(&MctpBench::VENDOR_SUBTYPE) {
            let _ =
                MctpBench::handle_request(msg, &mut resp, bench_request).await;
            continue;
        }

        if !msg.starts_with(&VENDOR_SUBTYPE_ECHO) {
            warn!("echo wrong vendor subtype");
            continue;
        }

        info!("echo msg len {} from eid {}", msg.len(), resp.remote_eid());
        if let Err(e) = resp.send(msg).await {
            warn!("listener reply fail {e}");
        } else {
            info!("replied");
        }
    }
}
