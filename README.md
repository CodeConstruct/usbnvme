# STM32 MCTP USB device

This implements a MCTP-over-USB device on a STM32H7S3L8 Nucleo board.

## Functionality

Current features are:

- MCTP Control Protocol
- mctp-echo and mctp-bench send functionality (`mctp-bench` as an optional feature).

Pending are NVMe-MI and other MCTP protocols.

## Requirements

Install `probe-rs` for interactions with the Nucleo board.

```
cargo install probe-rs-tools
```

or install a binary.

Building usbnvme requires a recent Rust, install it with `rustup` (not needed for flashing/debug logs).

### Static cross-compiiled probe-rs

A static probe-rs binary can be cross-compiled for embedded Linux ARM.
Cross-compiling a current (2025-04-29) probe-rs checkout with the following should work:

```sh
rustup target add armv7-unknown-linux-musleabihf

CC=clang  cargo build --release --bin probe-rs  \
    --target armv7-unknown-linux-musleabihf \
    --config 'patch.crates-io.udev.git="https://github.com/xobs/basic-udev.git"'  \
    --config strip=true
```
## Board setup

Attach a USB-C cable from the computer running `probe-rs` to `CN5 [STLK]` on the Nucleo -
`probe-rs` will interact with the on-board ST-Link debug interface. `probe-rs` can run
on either a development PC (when developing `usbnvme`), or run on a BMC to fetch debug logs
from the Nucleo board and flash new firmware out-of-band. Power is provided by CN5
(assuming default jumper config).

The MCTP-over-USB port is `CN2 [USB]`, attach that to the BMC. Once `usbnvme` is running
it will show up as `mctpusb0` etc (assuming the Linux driver is present).

## Building

```sh
cargo build --release
```

Debug builds (no `--release`) still have high optimisation level, but enable 
some additional assertions and set log level to `trace`.

## Flashing

As a one-time step, install the [`xspiloader`](xspiloader/README.md) bootloader following instructions.
`xspiloader` runs from internal STM32 flash, loading and running a program in SRAM copied from
the external SPI flash on the Nucleo board.

The actual MCTP application is flashed to the Nucleo board. Attach the nucleo st-link USB cable to
the computer.

```sh
# (any arm "strip" tool can be used, rust-strip is multiarch)
rust-strip target/thumbv7em-none-eabihf/release/usbnvme -o usbnvme-strip.elf

probe-rs download --chip STM32H7S3L8 --probe 0483:3754 \
    --chip-description-path xspiloader/chip-h7s3-nucleo.yaml \
    --binary-format bin --base-address 0x70000000  \
    usbnvme-strip.elf

```

After a reset the program should run. 

```sh
probe-rs reset --chip STM32H7S3L8 --probe 0483:3754

```

The board LED will blink slowly.

## Debug logs

Logs are provided over the ST-Link USB port, via a RTT channel.

```sh
probe-rs attach --chip STM32H7S3L8 --probe 0483:3754 --rtt-scan-memory /dev/null
```

The default release log level is `info`, for dev builds it is `trace`.

## Development

For development the program is run directly from SRAM (no flash or bootloader involved).

```sh
cargo run --release
```
