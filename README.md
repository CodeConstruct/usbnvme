# STM32 MCTP USB device

This implements a MCTP-over-USB device on a STM32H7S3L8 Nucleo board.

## Functionality

Current features are:

- MCTP Control Protocol
- `mctp-echo` test service
- optional `mctp-bench` benchmark service
- Debug log via USB CDC-ACM

Pending are NVMe-MI and other MCTP protocols.

When running with the usbnvme firmware, the Nucleo board provides USB
interfaces on two separate USB-C ports:

- the "MCTP" port (labelled `CN2 [USB]` on the board silkscreen)
- the "debug" port (labelled `CN5 [STLK]` on the board silkscreen)

The MCTP port exposes USB device (product:vendor ID `ccde`:`0000`), with two
functions:

- A MCTP-over-USB device, providing the core MCTP functionality
- A serial-over-USB device, providing device debug logs.

If necessary, the serial-over-USB device can be disabled by build-time
configuration.

The MCTP endpoint supports the MCTP control protocol, allowing EID assignement
and device enumeration.

For testing, the endpoint will respond to MCTP echo messages - a Code Construct
vendor message type, supported by the `mctp-req` utility at [MCTP
tools][https://github.com/CodeConstruct/mctp].

For benchmarking, `mctp-bench` (as a sender) is optionally supported, but
is disabled in the default build.

The debug port exposes a hardware ST-Link interface, allowing firmware upload,
chip debug and access to debug logs. These debug logs are a mirror of those from
the serial-over-USB device above.

## Requirements

Install Rust with [`rustup`](https://rustup.rs/).

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Build and install `probe-rs` for interactions with the Nucleo board.

```sh
cargo install probe-rs-tools
```

or [download](https://github.com/probe-rs/probe-rs/releases/latest) a binary.
See below for cross-compiling a static binary.

`probe-rs` requires permission to access the ST-Link device as non-root
(if running on a PC):

```sh
echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="0483", ATTRS{idProduct}=="3754", GROUP="plugdev", MODE="0660", TAG+="uaccess"' | sudo tee /etc/udev/rules.d/70-stlink.rules
sudo systemctl restart udev
```

## Nucleo board setup

Attach a USB-C cable from the computer running `probe-rs` to `CN5 [STLK]` on the Nucleo.
`probe-rs` will interact with the on-board ST-Link debug interface. `probe-rs` can run
on either a development PC (when developing/flashing `usbnvme`), or run on a BMC
to fetch debug logs from the Nucleo board and flash new firmware out-of-band.

In unprogrammed state, three LEDs (red, orange, green) will flash.

The MCTP-over-USB port is `CN2 [USB]`, attach that to the BMC. Once `usbnvme` is running
it will show up as `mctpusb0` etc (assuming the Linux driver is present).

Nucleo JP3 jumper configures the power source:

| Position | Source |
|---       | ---    |
| 1-2      | CN5 stlink (default) |
| 7-8      | CN2, MCTP-over-USB port |

## Building

In the `usbnvme` checkout directory:

```sh
cargo build --release
```

The output ELF firmware is `target/thumbv7em-none-eabihf/release/usbnvme`.

## Flashing

As a one-time step, install the [`xspiloader`](xspiloader/README.md) bootloader following instructions
in that directory.
`xspiloader` runs from internal STM32 flash. At boot it loads a ELF binary from external SPI flash to SRAM,
and runs it.

The `usbnvme` binary is is flashed to the Nucleo board. Attach the nucleo st-link USB cable to
the computer.

```sh
# (any arm "strip" tool can be used, rust-strip is multiarch)
rust-strip target/thumbv7em-none-eabihf/release/usbnvme -o usbnvme-strip.elf

probe-rs download --chip STM32H7S3L8 --probe 0483:3754 \
    --chip-description-path xspiloader/chip-h7s3-nucleo.yaml \
    --binary-format bin --base-address 0x70000000  \
    usbnvme-strip.elf

```

## Running

After a reset or power cycle the `usbnvme` firmware will run.

```sh
probe-rs reset --chip STM32H7S3L8 --probe 0483:3754
# (ignore warnings about not being halted)
```

The orange board LD2 LED will blink slowly.

`mctpusb0` device should be visible on the BMC with `mctp link`.

Assign an EID using mctpd over D-Bus:

```sh
busctl call  au.com.codeconstruct.MCTP1 /au/com/codeconstruct/mctp1/interfaces/mctpusb0 au.com.codeconstruct.MCTP.BusOwner1 SetupEndpoint ay 0
yisb 8 1 "/au/com/codeconstruct/mctp1/networks/1/endpoints/8" true

busctl introspect au.com.codeconstruct.MCTP1 /au/com/codeconstruct/mctp1/networks/1/endpoints/8
```

## Debug logs

Logs are provided over the ST-Link USB port, via a RTT channel:

```sh
probe-rs attach --chip STM32H7S3L8 --probe 0483:3754 --rtt-scan-memory /dev/null
# (ignore SwdApFault warnings)
```

The default release log level is `info`.

Logs can also be retrieved from a USB serial interface on the MCTP-USB port:

```sh
cat /dev/serial/by-id/usb-Code_Construct_usbnvme-0.1_1-if01
         0 INFO  usbnvme. device 4f7aaaa3-4b5e-41bb-ba2f-c21aac34dfe7
         0 INFO  mctp usb waiting
...
```

## Development

For development `usbnvme` is run directly from SRAM (no flash or bootloader involved).

```sh
cargo run --release
```
Omit the `--release` to add extra assertions/integer overflow checks and `debug` level logs,
at the expense of binary size.

## Device identifiers

Each board has a persistent UUID, reported by MCTP control protocol.
The first 12 digits of that UUID are used as the USB serial number.

That can be used to correlate the device ID printed at boot on the ST-Link debug log,
or the Linux usb-serial path.

```sh
$ grep . /sys/class/net/mctpusb*/device/../serial
/sys/class/net/mctpusb0/device/../serial:4f7aaaa34b
/sys/class/net/mctpusb1/device/../serial:9868840502

$ ls -l /dev/serial/by-id/*
lrwxrwxrwx    1 root     root            13 May 13 08:30 /dev/serial/by-id/usb-Code_Construct_usbnvme-0.1_4f7aaaa34b-if01 -> ../../ttyACM0
lrwxrwxrwx    1 root     root            13 May 13 08:30 /dev/serial/by-id/usb-Code_Construct_usbnvme-0.1_9868840502-if01 -> ../../ttyACM1

$ busctl call  au.com.codeconstruct.MCTP1 /au/com/codeconstruct/mctp1/interfaces/mctpusb0 au.com.codeconstruct.MCTP.BusOwner1 SetupEndpoint ay 0
yisb 8 1 "/au/com/codeconstruct/mctp1/networks/1/endpoints/8" false
$ busctl get-property   au.com.codeconstruct.MCTP1 /au/com/codeconstruct/mctp1/networks/1/endpoints/8 xyz.openbmc_project.Common.UUID UUID
s "4f7aaaa3-4b5e-41bb-ba2f-c21aac34dfe7"

```

When multiple devices are attached, a longer `probe-rs` argument is needed
to distinguish them, eg
```sh
probe-rs run --probe 0483:3754:003200303133510F35333335
```
The `probe-rs` ST-Link identifier is different to the MCTP-USB UUID.

## Static cross-compiled probe-rs

If probe-rs needs to run on an embedded Linux ARM system, it can be built statically.
Cross-compiling a current (2025-04-29) probe-rs checkout with the following should work:

```sh
rustup target add armv7-unknown-linux-musleabihf

git clone https://github.com/probe-rs/probe-rs
cd probe-rs

CC=clang  cargo build --release --bin probe-rs  \
    --target armv7-unknown-linux-musleabihf \
    --config 'patch.crates-io.udev.git="https://github.com/xobs/basic-udev.git"'  \
    --config strip=true
```

The built binary is `target/armv7-unknown-linux-musleabihf/release/probe-rs`.
