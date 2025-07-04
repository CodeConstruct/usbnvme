// SPDX-License-Identifier: GPL-3.0-only
/*
 * Copyright (c) 2025 Code Construct
 */
#[allow(unused)]
use log::{debug, error, info, trace, warn};

use core::fmt::Write;
use embassy_executor::Spawner;
use embassy_stm32::peripherals::USB_OTG_HS;
use embassy_stm32::usb::{DmPin, DpPin, Driver};
use embassy_stm32::{bind_interrupts, usb, Peri};
#[allow(unused_imports)]
use embassy_usb::{class::cdc_acm, Builder};
use heapless::String;
use mctp_estack::router::{PortBottom, PortId, Router};
use mctp_usb_embassy::{MctpUsbClass, MCTP_USB_MAX_PACKET};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    OTG_HS => usb::InterruptHandler<USB_OTG_HS>;
});

#[cfg(feature = "log-usbserial")]
type Endpoints = (
    MctpUsbClass<'static, Driver<'static, USB_OTG_HS>>,
    cdc_acm::CdcAcmClass<'static, Driver<'static, USB_OTG_HS>>,
);
#[cfg(not(feature = "log-usbserial"))]
type Endpoints = (MctpUsbClass<'static, Driver<'static, USB_OTG_HS>>,);

pub(crate) fn setup(
    spawner: Spawner,
    usb: Peri<'static, USB_OTG_HS>,
    dp: Peri<'static, impl DpPin<USB_OTG_HS>>,
    dm: Peri<'static, impl DmPin<USB_OTG_HS>>,
) -> Endpoints {
    let mut config = embassy_usb::Config::new(0x3834, 0x0000);
    config.manufacturer = Some("Code Construct");
    config.product = Some(crate::PRODUCT);

    // USB serial number matches the first 12 digits of the mctp uuid
    static SERIAL: StaticCell<String<{ uuid::fmt::Simple::LENGTH }>> =
        StaticCell::new();
    let serial = SERIAL.init(String::new());
    write!(serial, "{}", crate::device_uuid().simple()).unwrap();
    config.serial_number = Some(&serial[..12]);

    let driver_config = embassy_stm32::usb::Config::default();
    // TODO: is vbus detection needed? Seems not on the nucleo?
    // driver_config.vbus_detection = true;

    const CONTROL_SZ: usize = 64;
    const USBSERIAL_SZ: usize = 64;
    // TODO: +1 workaround can be removed once this merges:
    // https://github.com/embassy-rs/embassy/pull/3892
    const OUT_SZ: usize = MCTP_USB_MAX_PACKET + CONTROL_SZ + USBSERIAL_SZ + 1;
    static EP_OUT_BUF: StaticCell<[u8; OUT_SZ]> = StaticCell::new();

    let ep_out_buf = EP_OUT_BUF.init([0; OUT_SZ]);
    let driver = Driver::new_hs(usb, Irqs, dp, dm, ep_out_buf, driver_config);

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

    #[cfg(feature = "log-usbserial")]
    let ret = {
        static STATE: StaticCell<cdc_acm::State> = StaticCell::new();
        let state = STATE.init(Default::default());
        let serial = cdc_acm::CdcAcmClass::new(&mut builder, state, 64);
        (mctp, serial)
    };
    #[cfg(not(feature = "log-usbserial"))]
    let ret = (mctp,);

    let usb = builder.build();
    spawner.spawn(usb_task(usb)).unwrap();

    ret
}

#[embassy_executor::task]
async fn usb_task(
    mut usb: embassy_usb::UsbDevice<'static, Driver<'static, USB_OTG_HS>>,
) {
    usb.run().await
}

#[embassy_executor::task]
pub async fn usb_recv_task(
    router: &'static Router<'static>,
    usb_receiver: mctp_usb_embassy::Receiver<
        'static,
        Driver<'static, USB_OTG_HS>,
    >,
    port: PortId,
) {
    usb_receiver.run(router, port).await;
}

#[embassy_executor::task]
pub async fn usb_send_task(
    mctp_usb_bottom: PortBottom<'static>,
    usb_sender: mctp_usb_embassy::Sender<
        'static,
        Driver<'static, USB_OTG_HS>,
    >,
) {
    usb_sender.run(mctp_usb_bottom).await;
}
