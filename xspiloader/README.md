## XSPI Bootloader

Bootload an ELF program from external SPI flash. The program is copied
from flash to run in RAM.

Targets a stm32h7s3 nucleo board, stm32h7s3l8 with MX25UW25645GXDI00 flash.

## Installing the bootloader

Compile and write to internal flash:

```
cargo run --release
```

It will write the bootloader to internal flash and boot, though the bootloader might
fail to load a target ELF program if none exists yet.

## Target Program

The target ELF program must be linked to run from RAM, and must
set up stack and VTOR itself (cortex-m-rt `set-sp` and `set-vtor` features). The bootloader jumps to the ELF entrypoint address.

Make a stripped copy of the resultant ELF program (optional, 
recommended for size), then write it to external flash:

```
probe-rs download --chip-description-path chip-h7s3-nucleo.yaml --binary-format bin --base-address 0x70000000 --chip STM32H7S3L8 --probe 0483:3754 /path/to/program.stripped.elf
```

`chip-h7s3-nucleo.yaml` is a modified version of `probe-rs` [`STM32H7RS_Series.yaml`](https://github.com/probe-rs/probe-rs/blob/master/probe-rs/targets/STM32H7RS_Series.yaml),
with only the nucleo flash algorithm selected, and only `STM32H7R7L8`.
