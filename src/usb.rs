#[allow(unused)]
use log::{debug, error, info, trace, warn};

use embassy_executor::Spawner;
use embassy_stm32::peripherals::USB_OTG_HS;
use embassy_stm32::usb::{DmPin, DpPin, Driver};
use embassy_stm32::{bind_interrupts, usb, Peri};
use embassy_usb::Builder;
use mctp_usb_embassy::{MctpUsbClass, MCTP_USB_MAX_PACKET};
use static_cell::StaticCell;
use mctp_estack::router::{PortBottom, Router, PortId};

bind_interrupts!(struct Irqs {
    OTG_HS => usb::InterruptHandler<USB_OTG_HS>;
});

pub(crate) fn setup(
    spawner: Spawner,
    usb: Peri<'static, USB_OTG_HS>,
    dp: Peri<'static, impl DpPin<USB_OTG_HS>>,
    dm: Peri<'static, impl DmPin<USB_OTG_HS>>,
) -> MctpUsbClass<'static, Driver<'static, USB_OTG_HS>> {
    let mut config = embassy_usb::Config::new(0x0000, 0x0000);
    config.manufacturer = Some("Code Construct");
    config.product = Some("usbnvme-0.1");
    config.serial_number = Some("1");

    let driver_config = embassy_stm32::usb::Config::default();
    // TODO: is vbus detection needed? Seems not on the nucleo?
    // driver_config.vbus_detection = true;

    const CONTROL_SZ: usize = 64;
    // TODO: +1 workaround can be removed once this merges:
    // https://github.com/embassy-rs/embassy/pull/3892
    const OUT_SZ: usize = MCTP_USB_MAX_PACKET + CONTROL_SZ + 1;
    static EP_OUT_BUF: StaticCell<[u8; OUT_SZ]> = StaticCell::new();

    let ep_out_buf = EP_OUT_BUF.init([0; OUT_SZ]);
    let driver = Driver::new_hs(
        usb,
        Irqs,
        dp,
        dm,
        ep_out_buf,
        driver_config,
    );

    // UsbDevice will be static to pass to usb_task. That requires static buffers.
    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 32]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; CONTROL_SZ]> = StaticCell::new();
    let config_descriptor = CONFIG_DESCRIPTOR.init([0; 256]);
    let bos_descriptor = BOS_DESCRIPTOR.init([0; 32]);
    let control_buf = CONTROL_BUF.init([0; CONTROL_SZ]);

    let mut builder = Builder::new(
        driver,
        config,
        config_descriptor,
        bos_descriptor,
        &mut [],
        control_buf,
    );

    let mctp = MctpUsbClass::new(&mut builder);

    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();

    mctp
}

#[embassy_executor::task]
async fn usb_task(mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB_OTG_HS>>) {
    usb.run().await
}

#[embassy_executor::task]
pub async fn usb_recv_task(
    router: &'static Router<'static>,
    mut usb_receiver: mctp_usb_embassy::Receiver<
        'static,
        Driver<'static, USB_OTG_HS>,
    >,
    port: PortId,
) {
    // Outer loop for reattaching USB
    loop {
        info!("mctp usb waiting");
        usb_receiver.wait_connection().await;
        info!("mctp usb attached");

        // Inner loop receives packets and provides MCTP handling
        'receiving: loop {
            match usb_receiver.receive().await {
                Some(Ok(pkt)) => {
                    trace!("router recv len {}", pkt.len());
                    router.inbound(pkt, port).await;
                }
                Some(Err(e)) => debug!("mctp usb packet decode failure {}", e),
                None => {
                    info!("mctp usb disconnected");
                    break 'receiving;
                }
            }
        }
    }
}

#[embassy_executor::task]
pub async fn usb_send_task(
    mut mctp_usb_bottom: PortBottom<'static>,
    mut usb_sender: mctp_usb_embassy::Sender<'static, Driver<'static, USB_OTG_HS>>,
) {
    // Outer loop for reattaching USB
    loop {
        info!("mctp usb waiting");
        usb_sender.wait_connection().await;
        info!("mctp usb attached");
        'sending: loop {
            // Wait for at least one MCTP packet enqueued
            let (pkt, _dest) = mctp_usb_bottom.outbound().await;
            let r = usb_sender.feed(pkt);

            // Consume it
            mctp_usb_bottom.outbound_done();
            if r.is_err() {
                // MCTP packet too large for USB
                continue 'sending;
            }

            'fill: loop {
                let Some((pkt, _dest)) = mctp_usb_bottom.try_outbound() else {
                    // No more packets
                    break 'fill;
                };

                // See if it fits in the payload
                match usb_sender.feed(pkt) {
                    // Success, consume it
                    Ok(()) => mctp_usb_bottom.outbound_done(),
                    // Won't fit, leave it until next 'sending iteration.
                    Err(_) => break 'fill,
                }
            }

            if let Err(e) = usb_sender.flush().await {
                debug!("usb send error {}", e);
                break 'sending;
            }
        }
    }
}
