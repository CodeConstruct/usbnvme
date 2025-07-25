//! PLDM File Transfer requester.

// SPDX-License-Identifier: GPL-3.0-only
/*
 * Copyright (c) 2025 Code Construct
 */
#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use core::future::Future;

use pldm_file::PLDM_TYPE_FILE_TRANSFER;
use pldm_platform::proto::PdrRecord;

use embassy_futures::select::select;
use embassy_time::Duration;
use mctp::{AsyncReqChannel, Eid};
use mctp_estack::Router;
use pldm::control::{requester as ctrq, PLDM_TYPE_CONTROL};
use pldm::{proto_error, PldmError, PldmResult};
use pldm_platform::requester as platrq;

use crate::SignalCS;

pub struct PldmTimedout;
impl From<PldmTimedout> for PldmError {
    fn from(_: PldmTimedout) -> Self {
        proto_error!("Timed out")
    }
}

pub trait PldmTimeout: Future + Sized {
    /// Run a future with a timeout.
    async fn with_timeout(
        self,
        timeout: Duration,
    ) -> Result<Self::Output, PldmTimedout> {
        embassy_time::with_timeout(timeout, self)
            .await
            .map_err(|_| PldmTimedout)
    }
}

impl<F: Future> PldmTimeout for F {}

#[embassy_executor::task]
pub(crate) async fn pldm_file_task(
    router: &'static Router<'static>,
    peer: &'static SignalCS<Eid>,
) -> ! {
    info!("PLDM file task started");

    let mut host = None;

    loop {
        let target = match host.take() {
            Some(t) => t,
            None => peer.wait().await,
        };

        info!("Running PLDM file transfer from {target}");

        let run = async {
            if let Err(e) = pldm_run_file(target, router).await {
                warn!("Error running file transfer: {e}");
            }
        };

        // A subsequent Set Endpoint ID will interrupt the transfer.
        // TODO: Revisit this once we have timeouts
        let setendpoint = async {
            host = Some(peer.wait().await);
        };

        select(run, setendpoint).await;
    }
}

async fn check_version(
    comm: &mut impl AsyncReqChannel,
    pldm_type: u8,
    expect_version: u32,
) -> PldmResult<()> {
    let mut buf = [0u32; 10];

    trace!("check_version {pldm_type}");
    let r = ctrq::get_pldm_version(comm, pldm_type, &mut buf).await;
    match &r {
        Ok(versions) => {
            info!("PLDM type {pldm_type} versions: {versions:#08x?}");
            if !versions.contains(&expect_version) {
                // TODO bail?
                warn!("Expected version {expect_version:#08x} not supported");
            }
        }
        Err(e) => {
            warn!("Error from GetPLDMVersion type {pldm_type}: {e}");
        }
    };
    trace!("check_version done {r:?}");
    r.map(|_| ())
}

async fn check_commands(
    comm: &mut impl AsyncReqChannel,
    pldm_type: u8,
    version: u32,
    required_commands: &[u8],
) -> PldmResult<()> {
    let mut buf = [0u8; 50];

    let r = ctrq::get_pldm_commands(comm, pldm_type, version, &mut buf).await;
    match &r {
        Ok(cmds) => {
            info!("PLDM type {pldm_type} commands: {cmds:#02x?}");
            for c in required_commands {
                if !cmds.contains(c) {
                    warn!("Required command {c:#02x} missing");
                }
            }
        }
        Err(e) => {
            warn!("Error from GetPLDMVersion type 0: {e}");
        }
    };
    r.map(|_| ())
}

async fn pldm_run_file(
    eid: Eid,
    router: &'static Router<'static>,
) -> Result<(), PldmError> {
    use pldm_file::client::*;
    use pldm_file::proto::*;

    // TODO align with pldm crates?
    const PLDM_BASE_VERSION: u32 = 0xf1f1f000;
    const PLDM_FILE_VERSION: u32 = 0xf1f0f000;

    const SHORT_TIMEOUT: Duration = Duration::from_secs(4);
    const READ_TIMEOUT: Duration = Duration::from_secs(120);

    let mut comm = router.req(eid);
    let comm = &mut comm;

    // Set a fixed timeout for the first sequence
    let first_sequence = async {
        // Get PLDM Versions
        let _ = check_version(comm, PLDM_TYPE_CONTROL, PLDM_BASE_VERSION).await;
        let _ = check_version(
            comm,
            pldm_file::PLDM_TYPE_FILE_TRANSFER,
            PLDM_FILE_VERSION,
        )
        .await;

        // Get PLDM Types
        let mut buf = [0u8; 10];
        let types = ctrq::get_pldm_types(comm, &mut buf)
            .await
            .inspect_err(|e| warn!("Error from Get PLDM Types: {e}"))?;
        info!("PLDM types: {types:?}");
        if !(types.contains(&PLDM_TYPE_CONTROL)
            && types.contains(&PLDM_TYPE_FILE_TRANSFER))
        {
            warn!("Missing expected types");
        }

        // Get Commands type 0
        let required = [
            pldm::control::Cmd::NegotiateTransferParameters as u8,
            pldm::control::Cmd::MultipartReceive as u8,
        ];
        let _ = check_commands(
            comm,
            PLDM_TYPE_CONTROL,
            PLDM_BASE_VERSION,
            &required,
        );

        // Get Commands type 7
        let required = [
            pldm_file::proto::Cmd::DfOpen as u8,
            pldm_file::proto::Cmd::DfClose as u8,
            pldm_file::proto::Cmd::DfRead as u8,
        ];
        let _ = check_commands(
            comm,
            PLDM_TYPE_FILE_TRANSFER,
            PLDM_FILE_VERSION,
            &required,
        );

        // PDR Repository Info
        let pdr_info = platrq::get_pdr_repository_info(comm)
            .await
            .inspect_err(|e| {
                warn!("Error from Get PDR Repository Info: {e}")
            })?;

        info!("PDR Repository Info: {pdr_info:?}");

        // File Descriptor PDR
        let pdr_record = 1;
        let pdr = platrq::get_pdr(comm, pdr_record)
            .await
            .inspect_err(|e| warn!("Error from Get PDR: {e}"))?;

        let PdrRecord::FileDescriptor(filedesc) = pdr else {
            return Err(proto_error!("Not a file descriptor PDR: {pdr:#?}"));
        };
        info!("PDR: {filedesc:?}");
        // TODO: check PDR is as-expected

        // NegotiateTransferParameters
        let req_types = [pldm_file::PLDM_TYPE_FILE_TRANSFER];
        let (size, neg_types) = ctrq::negotiate_transfer_parameters(
            comm, &req_types, &mut buf, 1024,
        )
        .await
        .inspect_err(|e| warn!("Error from Negotiate: {e}"))?;
        info!("Negotiated multipart size {size} for types {neg_types:?}");
        Ok(filedesc)
    };

    // Whole first sequence runs with one timeout
    let filedesc = first_sequence
        .with_timeout(SHORT_TIMEOUT)
        .await
        .inspect_err(|_| warn!("PLDM file transfer setup timed out"))??;

    // File Open
    let id = FileIdentifier(filedesc.file_identifier);
    let attrs = DfOpenAttributes::empty();
    let fd = df_open(comm, id, attrs)
        .with_timeout(SHORT_TIMEOUT)
        .await?
        .inspect_err(|e| warn!("df_open failed {e}"))?;

    // File Read
    info!("Reading entire file ({} bytes)...", filedesc.file_max_size);
    let start = embassy_time::Instant::now();

    let mut count = 0;
    df_read_with(comm, fd, 0, filedesc.file_max_size as usize, |b| {
        count += b.len();
        Ok(())
    })
    .with_timeout(READ_TIMEOUT)
    .await?
    .inspect_err(|e| warn!("df_read failed {e}"))?;

    let time = start.elapsed().as_millis() as usize;
    let kbyte_rate = count / time;
    info!(
        "Received total {} bytes, {} ms, {} kB/s",
        count, time, kbyte_rate
    );

    // File Close
    let attrs = DfCloseAttributes::empty();
    df_close(comm, fd, attrs)
        .with_timeout(SHORT_TIMEOUT)
        .await?
        .inspect_err(|e| warn!("df_close failed {e}"))?;

    Ok(())
}
