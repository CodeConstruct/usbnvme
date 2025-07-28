// SPDX-License-Identifier: GPL-3.0-only
/*
 * Copyright (c) 2025 Code Construct
 */
#![no_std]
#![no_main]
// avoid mysterious missing awaits
#![deny(unused_must_use)]
#![deny(unsafe_op_in_unsafe_fn)]

use embassy_sync::signal::Signal;
#[allow(unused)]
use log::{debug, error, info, trace, warn};

use heapless::Vec;
use static_cell::StaticCell;

use embassy_executor::{Executor, InterruptExecutor, Spawner};
use embassy_futures::select::{select, Either};
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::{InterruptExt, Priority};
use embassy_stm32::{gpio, Config};
use embassy_time::{Duration, Instant, Timer};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use mctp::{AsyncListener, AsyncRespChannel};
use mctp::{Eid, MsgType};
use mctp_estack::control::ControlEvent;
use mctp_estack::router::{Port, PortId, PortLookup, PortTop, Router};

mod ccvendor;
mod multilog;
#[cfg(feature = "pldm-file")]
mod pldm;
mod stmutil;
mod usb;

use ccvendor::BenchRequest;

const USB_MTU: usize = 251;

// Optimal BENCH_LEN is (N*247 - 1).
// USB_MTU - 4, and one byte for MCTP message type.
// Even N are more efficient.
const BENCH_LEN: usize = 3951;
// const BENCH_LEN: usize = 987;
// const BENCH_LEN: usize = 246;
const _: () = assert!(BENCH_LEN >= 9);

// Simple panic handler
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    multilog::enter_panic();
    error!("panicked. {}", info);
    loop {}
}

static EXECUTOR_HIGH: InterruptExecutor = InterruptExecutor::new();
static EXECUTOR_MEDIUM: InterruptExecutor = InterruptExecutor::new();
static EXECUTOR_LOW: StaticCell<Executor> = StaticCell::new();

// UART5 and 4 are unused, so their interrupts are taken for the executors.
#[interrupt]
unsafe fn UART5() {
    unsafe { EXECUTOR_HIGH.on_interrupt() }
}

#[interrupt]
unsafe fn UART4() {
    unsafe { EXECUTOR_MEDIUM.on_interrupt() }
}

fn config() -> Config {
    use embassy_stm32::rcc::*;
    let mut config = embassy_stm32::Config::default();
    // 64MHz hsi_clk
    config.rcc.hsi = Some(HSIPrescaler::DIV1);
    config.rcc.hsi48 = Some(Hsi48Config {
        sync_from_usb: true,
    }); // needed for USB
    config.rcc.hse = None;

    config.rcc.pll1 = Some(Pll {
        source: PllSource::HSI,
        prediv: PllPreDiv::DIV16, // 4MHz (refN_ck range 1-16MHz)
        mul: PllMul::MUL150,
        divp: Some(PllDiv::DIV1), // 600 MHz
        divq: Some(PllDiv::DIV2), // 300 MHz
        divr: Some(PllDiv::DIV2), // 300 MHz
    });
    config.rcc.pll3 = Some(Pll {
        source: PllSource::HSI,
        prediv: PllPreDiv::DIV16, // 4MHz (refN_ck range 1-16MHz)
        mul: PllMul::MUL80,       // 320Mhz
        divp: Some(PllDiv::DIV10), // 32 MHz
        // 32MHz max for Usbphycsel
        divq: Some(PllDiv::DIV10), // 32 MHz
        divr: Some(PllDiv::DIV10), // 32 MHz
    });
    config.rcc.sys = Sysclk::PLL1_P; // 600 MHz
    config.rcc.ahb_pre = AHBPrescaler::DIV2; // 300 MHz
    config.rcc.apb1_pre = APBPrescaler::DIV2; // 150 MHz
    config.rcc.apb2_pre = APBPrescaler::DIV2; // 150 MHz
    config.rcc.apb4_pre = APBPrescaler::DIV2; // 150 MHz
    config.rcc.apb5_pre = APBPrescaler::DIV2; // 150 MHz
    config.rcc.voltage_scale = VoltageScale::HIGH;

    config.rcc.mux.usbphycsel = mux::Usbphycsel::PLL3_Q;
    // i3c1 uses default p1 = 150MHz. Good multiple of 12.5Mhz SCL clock.

    config
}

pub fn now() -> u64 {
    Instant::now().as_millis()
}

struct Routes {}

impl Routes {
    const USB_INDEX: PortId = PortId(0);
}

impl PortLookup for Routes {
    fn by_eid(
        &self,
        _eid: Eid,
        src_port: Option<PortId>,
    ) -> (Option<PortId>, Option<usize>) {
        if src_port == Some(Self::USB_INDEX) {
            // Avoid routing loops
            return (None, None);
        }
        // All packets out USB
        (Some(Self::USB_INDEX), Some(USB_MTU))
    }
}

/// Persistent UUID
///
/// This is generated based on the hardware device ID.
pub fn device_uuid() -> uuid::Uuid {
    let devid = stmutil::device_id();
    use hmac::Mac;
    let mut u = hmac::Hmac::<sha2::Sha256>::new_from_slice(&devid).unwrap();
    u.update(b"deviceid");
    let u = u.finalize().into_bytes();
    let u: [u8; 16] = u[..16].try_into().unwrap();

    uuid::Builder::from_random_bytes(u).into_uuid()
}

pub const PRODUCT: &str = concat!(
    "usbnvme",
    "-",
    env!("CARGO_PKG_VERSION"),
    "-",
    env!("GIT_REV")
);

#[cortex_m_rt::entry]
fn main() -> ! {
    let logger = multilog::init();
    info!("{}. device {}", PRODUCT, device_uuid().hyphenated());
    debug!("debug log enabled");
    trace!("trace log enabled");

    let executor = EXECUTOR_LOW.init(Executor::new());
    executor.run(|spawner| run(spawner, logger))
}

fn setup_mctp() -> (&'static Router<'static>, Port<'static>) {
    static USB_TOP: StaticCell<PortTop> = StaticCell::new();
    static LOOKUP: StaticCell<Routes> = StaticCell::new();
    static ROUTER: StaticCell<Router> = StaticCell::new();

    // USB port for the MCTP router
    let usb_top = USB_TOP.init_with(PortTop::new);

    // MCTP stack
    let lookup = LOOKUP.init(Routes {});
    // Router is large, using init_with() is important to construct in-place
    let router = ROUTER.init_with(|| Router::new(Eid(0), lookup, now()));
    let usb_id = router.add_port(usb_top).unwrap();
    debug_assert_eq!(usb_id, Routes::USB_INDEX);
    let usb_port = router.port(Routes::USB_INDEX).unwrap();

    (router, usb_port)
}

type SignalCS<T> = embassy_sync::signal::Signal<CriticalSectionRawMutex, T>;

fn run(low_spawner: Spawner, logger: &'static multilog::MultiLog) {
    // Highest priority goes to the USB send task, to fill the TX buffer
    // as quickly as possible once it becomes ready.
    //
    // Most other tasks run as medium.
    //
    // mctp-bench sender runs as low priority, so that other senders have a chance.
    // blinking LED is also low priority.

    // lower P number is higher priority (more urgent)
    interrupt::UART5.set_priority(Priority::P6);
    let high_spawner = EXECUTOR_HIGH.start(interrupt::UART5);

    interrupt::UART4.set_priority(Priority::P7);
    let medium_spawner = EXECUTOR_MEDIUM.start(interrupt::UART4);

    let p = embassy_stm32::init(config());

    let led = gpio::Output::new(p.PD13, gpio::Level::High, gpio::Speed::Low);

    /// Notification of the remote peer.
    ///
    /// Set on each Set Endpoint ID call. Initially None.
    static PEER_NOTIFY: SignalCS<Eid> = Signal::new();
    static USB_NOTIFY: SignalCS<bool> = Signal::new();
    static CONTROL_NOTIFY: SignalCS<ControlEvent> = Signal::new();
    static BENCH_REQUEST: SignalCS<BenchRequest> = Signal::new();

    let (router, mctp_usb_bottom) = setup_mctp();

    // MCTP over USB class device
    let endpoints =
        usb::setup(low_spawner, p.USB_OTG_HS, p.PM6, p.PM5, &USB_NOTIFY);

    #[cfg(feature = "log-usbserial")]
    let (mctpusb, usbserial) = endpoints;
    #[cfg(not(feature = "log-usbserial"))]
    let (mctpusb,) = endpoints;

    let (usb_sender, usb_receiver) = mctpusb.split();

    let echo = echo_task(router, &BENCH_REQUEST);
    let timeout = timeout_task(router);
    let control = control_task(router, &CONTROL_NOTIFY);
    let usb_send_loop = usb::usb_send_task(mctp_usb_bottom, usb_sender);
    let usb_recv_loop =
        usb::usb_recv_task(router, usb_receiver, Routes::USB_INDEX);
    let app_loop = usbnvme_app_task(&USB_NOTIFY, &CONTROL_NOTIFY, &PEER_NOTIFY);

    low_spawner.must_spawn(blink_task(led));
    medium_spawner.must_spawn(echo);
    medium_spawner.must_spawn(timeout);
    medium_spawner.must_spawn(usb_recv_loop);
    medium_spawner.must_spawn(control);
    medium_spawner.must_spawn(app_loop);
    // high priority for usb send
    high_spawner.must_spawn(usb_send_loop);

    #[cfg(feature = "nvme-mi")]
    {
        let nvmemi = nvme_mi_task(router);
        medium_spawner.must_spawn(nvmemi);
    }
    #[cfg(feature = "pldm-file")]
    {
        let pldm_file = pldm::pldm_file_task(router, &PEER_NOTIFY);
        medium_spawner.must_spawn(pldm_file);
    }
    #[cfg(feature = "mctp-bench")]
    {
        let bench = bench_task(router, &BENCH_REQUEST);
        low_spawner.must_spawn(bench);
    }
    let _ = logger;
    #[cfg(feature = "log-usbserial")]
    {
        let (sender, _) = usbserial.split();
        let seriallog = multilog::log_usbserial_task(sender, logger);
        low_spawner.must_spawn(seriallog);
    }
}

/// Task to handle usbnvme state transitions.
#[allow(unused)]
#[embassy_executor::task]
async fn usbnvme_app_task(
    usb_state_notify: &'static SignalCS<bool>,
    control_notify: &'static SignalCS<ControlEvent>,
    peer_watch: &'static SignalCS<Eid>,
) -> ! {
    let mut usb_state = false;
    loop {
        // Wait for either
        // - usb up/down event
        // - Set Endpoint ID from a bus owner.
        match select(usb_state_notify.wait(), control_notify.wait()).await {
            Either::First(s) => {
                info!("USB state -> {s:?}");
                usb_state = s;
            }
            Either::Second(ev) => match ev {
                // TODO: if more event variants are added, we may need to replace Signal
                // with a >1 sized Channel to ensure we don't lose events.
                ControlEvent::SetEndpointId {
                    old,
                    new,
                    bus_owner,
                } => {
                    info!("Own EID changed {old} -> {new} by bus owner {bus_owner}");
                    peer_watch.signal(bus_owner);
                }
            },
        }
    }
}

#[allow(unused)]
#[embassy_executor::task]
async fn echo_task(
    router: &'static mctp_estack::Router<'static>,
    bench_request: &'static SignalCS<BenchRequest>,
) -> ! {
    ccvendor::listener(router, bench_request).await
}

/// Checks timeouts in the MCTP stack.
#[embassy_executor::task]
async fn timeout_task(router: &'static mctp_estack::Router<'static>) -> ! {
    loop {
        let n = now();
        let delay = router.update_time(n).await.expect("time goes forwards");
        Timer::at(Instant::from_millis(delay + n)).await
    }
}

#[embassy_executor::task]
async fn control_task(
    router: &'static Router<'static>,
    control_notify: &'static SignalCS<ControlEvent>,
) -> ! {
    let mut l = router
        .listener(mctp::MCTP_TYPE_CONTROL)
        .expect("control listener");
    let mut c = mctp_estack::control::MctpControl::new(router);

    let mut types = Vec::<MsgType, 4>::new();
    types.push(mctp::MCTP_TYPE_CONTROL).unwrap();
    #[cfg(feature = "nvme-mi")]
    types.push(mctp::MCTP_TYPE_NVME).unwrap();

    c.set_message_types(&types).unwrap();
    c.set_uuid(&device_uuid());

    info!("MCTP Control Protocol server listening");
    let mut buf = [0u8; 256];
    loop {
        let Ok((_typ, _ic, msg, resp)) = l.recv(&mut buf).await else {
            warn!("control recv err");
            continue;
        };
        info!(
            "control recv len {} from eid {}",
            msg.len(),
            resp.remote_eid()
        );

        match c.handle_async(msg, resp).await {
            Ok(None) => (),
            Ok(Some(ev)) => control_notify.signal(ev),
            Err(e) => {
                warn!("control handler error: {e}");
            }
        }
    }
}

#[cfg(feature = "nvme-mi")]
#[embassy_executor::task]
async fn nvme_mi_task(router: &'static Router<'static>) -> ! {
    use nvme_mi_dev::*;
    let mut l = router
        .listener(mctp::MCTP_TYPE_NVME)
        .expect("NVME-MI listener");

    let mut subsys = Subsystem::new(SubsystemInfo::environment());
    let ppid = subsys.add_port(PortType::Pcie(PciePort::new())).unwrap();
    let ctrlid0 = subsys.add_controller(ppid).unwrap();
    let _ctrlid1 = subsys.add_controller(ppid).unwrap();

    let size_blocks = 10_000_000_000_000_u64.div_ceil(512);
    let nsid = subsys.add_namespace(size_blocks).unwrap();
    subsys
        .controller_mut(ctrlid0)
        .attach_namespace(nsid)
        .unwrap();

    let twpid = subsys
        .add_port(PortType::TwoWire(TwoWirePort::new()))
        .unwrap();
    let mut mep = ManagementEndpoint::new(twpid);

    debug!("NVMe-MI endpoint listening");

    let mut buf = [0u8; mctp_estack::config::MAX_PAYLOAD];
    loop {
        let Ok((_typ, ic, msg, resp)) = l.recv(&mut buf).await else {
            debug!("recv() failed");
            continue;
        };

        debug!("Handling NVMe-MI message: {msg:x?}");
        mep.handle_async(&mut subsys, msg, ic, resp, async |cmd| match cmd {
            CommandEffect::SetMtu { port_id, mtus } => {
                if port_id == ppid {
                    // TODO: implement once PortLookup::by_eid trait takes a
                    // non-mut reference.
                    warn!("NVMe-MI: Set MTU Port ID {port_id:?} MTU {mtus}, not currently handled");
                    Err(CommandEffectError::Unsupported)
                } else {
                    warn!("NVMe-MI: Set MTU bad Port ID {port_id:?}");
                    Err(CommandEffectError::InternalError)
                }
            }
            CommandEffect::SetSmbusFreq { .. } => {
                info!("NVMe-MI: Ignoring Set SMBUS Frequency");
                Err(CommandEffectError::Unsupported)
            }
        })
        .await;
    }
}

/// A mctp-bench sender.
///
/// Use with `mctp-bench` test tool from
/// <https://github.com/CodeConstruct/mctp>
#[allow(unused)]
#[embassy_executor::task]
async fn bench_task(
    router: &'static mctp_estack::Router<'static>,
    bench_trigger: &'static SignalCS<BenchRequest>,
) -> ! {
    debug!("mctp-bench send running");

    static BUF: StaticCell<[u8; BENCH_LEN]> = StaticCell::new();
    let buf = BUF.init_with(|| [0u8; BENCH_LEN]);

    let mut bench = ccvendor::MctpBench::new(buf).unwrap();

    let mut next_req = None;

    loop {
        let bench_req = match next_req.take() {
            Some(r) => r,
            None => bench_trigger.wait().await,
        };

        let mut req = router.req(bench_req.dest);
        req.tag_noexpire().unwrap();

        info!(
            "mctp-bench started to EID {}, {} messages, size {}",
            bench_req.dest, bench_req.count, bench_req.len
        );
        let send = async {
            if let Err(e) =
                bench.send(&mut req, bench_req.count, bench_req.len).await
            {
                warn!("bench failed: {e}");
            }
            info!(
                "mctp-bench sent {} iterations successfully",
                bench_req.count
            );
        };

        // Cancel the send loop when we receive a new request.
        let stopped = async {
            debug_assert!(next_req.is_none());
            next_req = Some(bench_trigger.wait().await);
            debug!("New bench request");
        };

        select(send, stopped).await;
    }
}

#[embassy_executor::task]
pub(crate) async fn blink_task(mut led: gpio::Output<'static>) {
    loop {
        trace!("led high");
        led.set_high();
        Timer::after(Duration::from_millis(2000)).await;

        trace!("led low");
        led.set_low();
        Timer::after(Duration::from_millis(2000)).await;
    }
}
