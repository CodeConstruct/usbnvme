#![no_std]
#![no_main]

#[cfg(feature = "defmt")]
#[allow(unused)]
use defmt::{debug, error, info, trace, warn};
#[cfg(feature = "defmt")]
use defmt_rtt as _;

use core::num::Wrapping;

use static_cell::StaticCell;

use embassy_executor::{Executor, InterruptExecutor, SendSpawner, Spawner};
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::{InterruptExt, Priority};
use embassy_stm32::{gpio, Config};
use embassy_time::{Duration, Instant, Timer};

use mctp::Eid;
use mctp::{AsyncListener, AsyncReqChannel, AsyncRespChannel};
use mctp_estack::router::{
    PortBuilder, PortLookup, PortStorage, PortTop, Router, PortId,
};

mod usb;

// TODO
const USB_MTU: usize = 251;
// const USB_MTU: usize = 5;
// const USB_MTU: usize = 128-4;

const BENCH_LEN: usize = 987;
// const BENCH_LEN: usize = 959;
// const BENCH_LEN: usize = 246;
// const BENCH_LEN: usize = 493;
// const BENCH_LEN: usize = 6;
const _: () = assert!(BENCH_LEN >= 6);

// use panic_probe as _;

// Simple panic handler without details saves 10+kB.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    error!("panicked. {}", defmt::Display2Format(info));
    loop {}
}

fn config() -> Config {
    use embassy_stm32::rcc::*;
    info!("config");
    let mut config = embassy_stm32::Config::default();
    // 64MHz hsi_clk
    config.rcc.hsi = Some(HSIPrescaler::DIV1);
    config.rcc.hsi48 = Some(Hsi48Config { sync_from_usb: true }); // needed for USB
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
        mul: PllMul::MUL80, // 320Mhz
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

fn now() -> u64 {
    Instant::now().as_millis()
}

struct Routes {
    // routing table goes here
}

impl Routes {
    const USB_INDEX: PortId = PortId(0);
}

impl PortLookup for Routes {
    fn by_eid(&mut self, _eid: Eid, _src_port: Option<PortId>) -> Option<PortId> {
        // TODO routing table
        Some(Self::USB_INDEX)
    }
}

static EXECUTOR_HIGH: InterruptExecutor = InterruptExecutor::new();
static EXECUTOR_LOW: StaticCell<Executor> = StaticCell::new();

#[interrupt]
unsafe fn UART5() {
    EXECUTOR_HIGH.on_interrupt()
}

#[cortex_m_rt::entry]
fn main() -> ! {
    info!("usbnvme");
    trace!("usbnvme trace");

    // const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Trace;
    // static LOGGER: rtt_logger::RTTLogger = rtt_logger::RTTLogger::new(LOG_LEVEL);
    // rtt_target::rtt_init_print!();
    // log::set_logger(&LOGGER)
    // .map(|()| log::set_max_level(LOG_LEVEL))
    // .unwrap();
    // log::info!("logger");

    interrupt::UART5.set_priority(Priority::P6);
    let high_spawner = EXECUTOR_HIGH.start(interrupt::UART5);

    let executor = EXECUTOR_LOW.init(Executor::new());
    executor.run(|spawner| run(spawner, high_spawner))
}

fn run(spawner: Spawner, high_spawner: SendSpawner) {
    let p = embassy_stm32::init(config());

    let led = gpio::Output::new(p.PD13, gpio::Level::High, gpio::Speed::Low);

    // MCTP over USB class device
    let mctpusb = usb::setup(spawner, p.USB_OTG_HS, p.PM6, p.PM5);

    static USB_PORT_STORAGE: StaticCell<PortStorage<4>> = StaticCell::new();
    static USB_PORT: StaticCell<PortBuilder> = StaticCell::new();

    static PORTS: StaticCell<[PortTop; 1]> = StaticCell::new();
    static LOOKUP: StaticCell<Routes> = StaticCell::new();
    static ROUTER: StaticCell<Router> = StaticCell::new();

    // USB port for the MCTP routing
    let usb_port_storage = USB_PORT_STORAGE.init(PortStorage::new());
    let usb_port = USB_PORT.init(PortBuilder::new(usb_port_storage));
    let (mctp_usb_top, mctp_usb_bottom) = usb_port.build(USB_MTU).unwrap();

    let ports = PORTS.init([
        mctp_usb_top,
    ]);

    let max_mtu = USB_MTU;
    let stack = mctp_estack::Stack::new(Eid(10), max_mtu, now());
    let lookup = LOOKUP.init(Routes {});
    let router = ROUTER.init(Router::new(stack, ports, lookup));

    let (usb_sender, usb_receiver) = mctpusb.split();

    let echo = echo_task(router);
    let timeout = timeout_task(router);
    let bench = bench_task(router);
    let usb_send_loop = usb::usb_send_task(mctp_usb_bottom, usb_sender);
    let usb_recv_loop = usb::usb_recv_task(router, usb_receiver, Routes::USB_INDEX);

    spawner.spawn(blink_task(led)).unwrap();
    spawner.spawn(bench).unwrap();
    spawner.spawn(echo).unwrap();
    spawner.spawn(timeout).unwrap();
    spawner.spawn(usb_recv_loop).unwrap();
    // high priority for usb send
    high_spawner.spawn(usb_send_loop).unwrap();
}

#[embassy_executor::task]
async fn echo_task(router: &'static mctp_estack::Router<'static>) -> ! {
    // mctp-echo is type 1, pldm
    let mut l = router.listener(mctp::MCTP_TYPE_PLDM).unwrap();
    let mut buf = [0u8; 100];
    loop {
        let Ok((msg, mut resp, _tag, typ, _ic)) = l.recv(&mut buf).await else {
            trace!("echo Bad listener recv");
            continue;
        };

        debug!("echo msg len {}", msg.len());
        if let Err(_e) = resp.send(typ, msg).await {
            trace!("listener reply fail");
        } else {
            trace!("replied");
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

/// A mctp-bench sender.
#[embassy_executor::task]
async fn bench_task(router: &'static mctp_estack::Router<'static>) -> ! {
    const MAGIC: u16 = 0xbeca;
    const SEQ_START: u32 = u32::MAX - 5;

    let mut buf = [0u8; BENCH_LEN];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }
    buf[..2].copy_from_slice(&MAGIC.to_le_bytes());

    let mut counter = Wrapping(SEQ_START);

    let mut req = router.req(Eid(20));
    req.tag_noexpire().unwrap();

    loop {
        buf[2..6].copy_from_slice(&counter.0.to_le_bytes());
        // if counter.0 % 30000 == 1 {
        //     info!("b {:02x}", buf);
        // }
        counter += 1;

        let r = req.send(mctp::MCTP_TYPE_PLDM, &buf).await;
        if let Err(e) = r {
            trace!("Error! {}", e);
        }
    }
}

#[embassy_executor::task]
pub(crate) async fn blink_task(mut led: gpio::Output<'static>) {
    loop {
        info!("high");
        led.set_high();
        Timer::after(Duration::from_millis(2000)).await;

        trace!("low");
        led.set_low();
        Timer::after(Duration::from_millis(2000)).await;
    }
}
