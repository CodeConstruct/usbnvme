// SPDX-License-Identifier: GPL-3.0-only
/*
 * Copyright (c) 2025 Code Construct
 */
#![no_std]
#![no_main]

#[allow(unused)]
use log::{debug, error, info, trace, warn};

use core::num::Wrapping;

use heapless::Vec;
use static_cell::StaticCell;

use embassy_executor::{Executor, InterruptExecutor, Spawner};
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::{InterruptExt, Priority};
use embassy_stm32::{gpio, Config};
use embassy_time::{Duration, Instant, Timer};

use mctp::{AsyncListener, AsyncReqChannel, AsyncRespChannel};
use mctp::{Eid, MsgType};
use mctp_estack::router::{
    PortBottom, PortBuilder, PortId, PortLookup, PortStorage, PortTop, Router,
};

mod multilog;
mod stmutil;
mod usb;

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
    EXECUTOR_HIGH.on_interrupt()
}

#[interrupt]
unsafe fn UART4() {
    EXECUTOR_MEDIUM.on_interrupt()
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
        &mut self,
        _eid: Eid,
        src_port: Option<PortId>,
    ) -> Option<PortId> {
        if src_port == Some(Self::USB_INDEX) {
            // Avoid routing loops
            return None;
        }
        // All packets out USB
        Some(Self::USB_INDEX)
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
    multilog::init();
    info!("{}. device {}", PRODUCT, device_uuid().hyphenated());
    debug!("debug log enabled");
    trace!("trace log enabled");

    let executor = EXECUTOR_LOW.init(Executor::new());
    executor.run(run)
}

fn setup_mctp() -> (&'static mut Router<'static>, PortBottom<'static>) {
    static USB_PORT_STORAGE: StaticCell<PortStorage<4>> = StaticCell::new();
    static USB_PORT: StaticCell<PortBuilder> = StaticCell::new();

    static PORTS: StaticCell<[PortTop; 1]> = StaticCell::new();
    static LOOKUP: StaticCell<Routes> = StaticCell::new();
    static ROUTER: StaticCell<Router> = StaticCell::new();

    // USB port for the MCTP router
    let usb_port_storage = USB_PORT_STORAGE.init_with(PortStorage::new);
    let usb_port = USB_PORT.init_with(|| PortBuilder::new(usb_port_storage));
    let (mctp_usb_top, mctp_usb_bottom) = usb_port.build(USB_MTU).unwrap();

    let ports = PORTS.init([mctp_usb_top]);

    // MCTP stack
    let max_mtu = USB_MTU;
    let lookup = LOOKUP.init(Routes {});
    // Router+Stack is large, using init_with() is important to construct in-place
    let router = ROUTER.init_with(|| {
        let stack = mctp_estack::Stack::new(Eid(0), max_mtu, now());
        Router::new(stack, ports, lookup)}
    );

    (router, mctp_usb_bottom)
}

fn run(spawner: Spawner) {
    let p = embassy_stm32::init(config());

    let led = gpio::Output::new(p.PD13, gpio::Level::High, gpio::Speed::Low);

    let (router, mctp_usb_bottom) = setup_mctp();

    // MCTP over USB class device
    let endpoints = usb::setup(spawner, p.USB_OTG_HS, p.PM6, p.PM5);

    #[cfg(feature = "log-usbserial")]
    let (mctpusb, usbserial) = endpoints;
    #[cfg(not(feature = "log-usbserial"))]
    let (mctpusb,) = endpoints;

    let (usb_sender, usb_receiver) = mctpusb.split();

    let echo = echo_task(router);
    let timeout = timeout_task(router);
    let control = control_task(router);
    let usb_send_loop = usb::usb_send_task(mctp_usb_bottom, usb_sender);
    let usb_recv_loop =
        usb::usb_recv_task(router, usb_receiver, Routes::USB_INDEX);

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

    spawner.spawn(blink_task(led)).unwrap();
    medium_spawner.spawn(echo).unwrap();
    medium_spawner.spawn(timeout).unwrap();
    medium_spawner.spawn(usb_recv_loop).unwrap();
    medium_spawner.spawn(control).unwrap();
    // high priority for usb send
    high_spawner.spawn(usb_send_loop).unwrap();

    #[cfg(feature = "nvme-mi")]
    {
        let nvmemi = nvme_mi_task(router);
        medium_spawner.spawn(nvmemi).unwrap();
    }
    #[cfg(feature = "mctp-bench")]
    {
        let bench = bench_task(router);
        spawner.spawn(bench).unwrap();
    }
    #[cfg(feature = "log-usbserial")]
    {
        let (sender, _) = usbserial.split();
        let seriallog = multilog::log_usbserial_task(sender);
        spawner.spawn(seriallog).unwrap();
    }
}

#[allow(unused)]
#[embassy_executor::task]
async fn echo_task(router: &'static mctp_estack::Router<'static>) -> ! {
    const VENDOR_SUBTYPE_ECHO: [u8; 3] = [0xcc, 0xde, 0xf0];
    let mut l = router.listener(mctp::MCTP_TYPE_VENDOR_PCIE).unwrap();
    let mut buf = [0u8; 100];
    loop {
        let Ok((_typ, _ic, msg, mut resp)) = l.recv(&mut buf).await else {
            warn!("echo Bad listener recv");
            continue;
        };

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
async fn control_task(router: &'static Router<'static>) -> ! {
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
        info!("control recv len {} from eid {}", msg.len(), resp.remote_eid());

        let r = c.handle_async(msg, resp).await;

        if let Err(e) = r {
            warn!("control handler failure: {}", e);
        }
    }
}

#[cfg(feature = "nvme-mi")]
#[embassy_executor::task]
async fn nvme_mi_task(router: &'static Router<'static>) -> ! {
    use nvme_mi_dev::nvme::*;
    let mut l = router
        .listener(mctp::MCTP_TYPE_NVME)
        .expect("NVME-MI listener");

    let mut subsys = Subsystem::new(SubsystemInfo::environment());
    let ppid = subsys.add_port(PortType::PCIe(PCIePort::new())).unwrap();
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
        mep.handle_async(&mut subsys, msg, ic, resp).await;
    }
}

/// A mctp-bench sender.
///
/// Use with `mctp-bench` test tool from
/// https://github.com/CodeConstruct/mctp. Asssumes receiver EID 90.
#[allow(unused)]
#[embassy_executor::task]
async fn bench_task(router: &'static mctp_estack::Router<'static>) -> ! {
    debug!("mctp-bench send running");
    const VENDOR_SUBTYPE_BENCH: [u8; 3] = [0xcc, 0xde, 0xf1];
    const MAGIC: u16 = 0xbeca;
    const SEQ_START: u32 = u32::MAX - 5;

    static BUF: StaticCell<[u8; BENCH_LEN]> = StaticCell::new();

    let buf = BUF.init_with(|| [0u8; BENCH_LEN]);
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }
    buf[..3].copy_from_slice(&VENDOR_SUBTYPE_BENCH);
    buf[3..5].copy_from_slice(&MAGIC.to_le_bytes());

    let mut counter = Wrapping(SEQ_START);

    let mut req = router.req(Eid(90));
    req.tag_noexpire().unwrap();

    loop {
        buf[5..9].copy_from_slice(&counter.0.to_le_bytes());
        counter += 1;

        let r = req.send(mctp::MCTP_TYPE_VENDOR_PCIE, buf).await;
        if let Err(e) = r {
            trace!("Error! {}", e);
        }
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
