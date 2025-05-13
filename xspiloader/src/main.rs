// SPDX-License-Identifier: MIT OR Apache-2.0
/*
 * Copyright (c) 2025 Code Construct
 */

/* "FlashMemory" based on Embassy examples,
 * Licensed as Apache-2.0 or MIT.
 */
#![no_std]
#![no_main]

use core::arch::asm;
use core::cell::RefCell;

#[allow(unused)]
use log::{debug, error, info, trace, warn};

use embassy_executor::Spawner;

use embassy_stm32::Config;
use embassy_stm32::mode::Blocking;
use embassy_stm32::pac;
use embassy_stm32::xspi::{
    AddressSize, ChipSelectHighTime, DummyCycles, FIFOThresholdLevel, Instance,
    MemorySize, MemoryType, TransferConfig, WrapSize, Xspi, XspiWidth,
};

use panic_probe as _;

const FLASH_SIZE: usize = 32 * 1024 * 1024;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    rtt_target::rtt_init_log!();

    info!("xspiloader stm32 bootloader.");
    info!("Loading ELF from external flash...");

    // RCC config
    // Default 64MHz is adequate
    let config = Config::default();
    // Initialize peripherals
    let p = embassy_stm32::init(config);

    /* Set ITCM/SRAM1 split to 128/64kB, DTCM/SRAM3 to 64/128kB */
    set_tcm_split(TCMSplit::Tcm128, TCMSplit::Tcm64);

    let qspi_config = embassy_stm32::xspi::Config {
        fifo_threshold: FIFOThresholdLevel::_4Bytes,
        memory_type: MemoryType::Macronix,
        delay_hold_quarter_cycle: true,
        device_size: MemorySize::_32MiB,
        chip_select_high_time: ChipSelectHighTime::_2Cycle,
        free_running_clock: false,
        clock_mode: false,
        wrap_size: WrapSize::None,
        // 64MHz
        clock_prescaler: 0,
        sample_shifting: false,
        chip_select_boundary: 0,
        max_transfer: 0,
        refresh: 0,
    };

    let xspi = embassy_stm32::xspi::Xspi::new_blocking_quadspi(
        p.XSPI2,
        p.PN6,
        p.PN2,
        p.PN3,
        p.PN4,
        p.PN5,
        p.PN1,
        qspi_config,
    );

    let flash = FlashMemory::new(xspi).await;
    let flash = FlashCell {
        inner: RefCell::new(flash),
    };

    let entry = load_elf(&flash).await.expect("elf loading failed");

    // Drop it to disable the XSPI peripheral.
    drop(flash);

    info!("booting (reattach probe-rs now) ...");
    log::logger().flush();

    // Clear rtt-target magic string, so that `probe-rs --rtt-scan-memory`
    // doesn't find the defunct bootloader
    unsafe extern "C" {
        #[link_name = "_SEGGER_RTT"]
        static mut SEGGER_RTT: [u8; 16];
    }
    let rtt_magic = &raw mut SEGGER_RTT;
    unsafe {
        rtt_magic.write_volatile([0; 16]);
    }

    unsafe {
        asm!(
            "bx {entry}",
            entry = in(reg) entry,
            options(noreturn, nomem, nostack),
        );
    }
}

/// `?TCM` gets this much memory, the `SRAM?` gets the rest.
#[allow(unused)]
enum TCMSplit {
    Tcm64 = 0b000,
    Tcm128 = 0b001,
    Tcm192 = 0b010,
}

/// Set ITCM/SRAM1 and DTCM/SRAM3 split.
fn set_tcm_split(itcm: TCMSplit, dtcm: TCMSplit) {
    let regs = pac::FLASH;

    // Unlock FLASH_OPTCR if necessary.
    // Unlocking twice would cause a busfault.
    if regs.optcr().read().optlock() {
        regs.optkeyr().write(|r| {
            r.set_ocukey(0x0819_2A3B);
        });
        regs.optkeyr().write(|r| {
            r.set_ocukey(0x4C5D_6E7F);
        });
    }

    // set PG_OPT to allow writing option bits
    regs.optcr().modify(|r| {
        r.set_pg_opt(true);
    });

    // set the split in option bytes
    pac::FLASH.obw2srp().modify(|r| {
        r.set_itcm_axi_share(itcm as u8);
        r.set_dtcm_axi_share(dtcm as u8);
    });

    // wait
    while regs.sr().read().qw() {}

    // clear PG_OPT and OPTLOCK
    regs.optcr().modify(|r| {
        r.set_pg_opt(true);
        r.set_optlock(true);
    });
}

/// Check whether a load address is valid
fn valid_dest(start: u32, length: u32) -> bool {
    let range = [
        // ITCM/SRAM1 and DTCM/SRAM3 split is configurable, these are upper limits.
        // Can't have the full range of both at once.

        // ITCM
        0x0000_0000..0x0003_0000,
        // SRAM1
        0x2400_0000..0x2402_0000,
        // DTCM
        0x2000_0000..0x2003_0000,
        // SRAM3
        0x2404_0000..0x2406_0000,
        // SRAM2 is used by xspiloader itself (link-bootloader.x), so disallowed.
    ];

    if length == 0 {
        return true;
    }

    let Some(end) = start.checked_add(length) else {
        return false;
    };

    for r in range {
        if r.contains(&start) && r.contains(&(end - 1)) {
            return true;
        }
    }
    false
}

fn neotron_error<T: core::fmt::Debug>(
    e: &neotron_loader::Error<T>,
) -> &'static str {
    use neotron_loader::Error;
    match e {
        Error::NotAnElfFile => "Not an ELF file",
        Error::Source(_) => "Error reading flash",
        Error::WrongElfFile => "Wrong ELF format",
        _ => "Other error",
    }
}

/// Loads an elf image.
///
/// Returns the entry address
async fn load_elf(
    source: impl neotron_loader::Source + Copy,
) -> Result<u32, ()> {
    let loader = neotron_loader::Loader::new(source).map_err(|e| {
        warn!("ELF loader failed: {}", neotron_error(&e));
    })?;

    for (idx, ph) in loader.iter_program_headers().enumerate() {
        let Ok(ph) = ph else {
            warn!("program header {} failed", idx);
            return Err(());
        };

        // Attempt to load PT_LOAD segments
        if ph.p_type() == neotron_loader::ProgramHeader::PT_LOAD {
            info!(
                "loading 0x{:x} len 0x{:x} from 0x{:x}",
                ph.p_paddr(),
                ph.p_memsz(),
                ph.p_offset()
            );
            // Flush in case it faults
            log::logger().flush();

            if !valid_dest(ph.p_paddr(), ph.p_memsz()) {
                error!("Invalid dest");
                return Err(());
            }

            if ph.p_memsz() == 0 {
                continue;
            }

            let (foff, addr, sz) = if ph.p_paddr() != 0 {
                (ph.p_offset(), ph.p_paddr(), ph.p_memsz())
            } else {
                // Rust disallows NULL pointers, which is unfortunate given
                // 0x0 is the start of ITCM where reset vectors can go.
                // Write the first byte specially using asm.
                let mut b = 0u8;
                if source
                    .read(ph.p_offset(), core::slice::from_mut(&mut b))
                    .is_err()
                {
                    error!("Failed reading");
                    return Err(());
                }
                unsafe {
                    asm!(
                        "strb {b}, [{zero}]",
                        b = in(reg) b,
                        zero = in(reg) 0,
                    );
                }

                (ph.p_offset() + 1, ph.p_paddr() + 1, ph.p_memsz() - 1)
            };

            let dest = (addr as usize) as *mut u8;
            let dest: &mut [u8] =
                unsafe { core::slice::from_raw_parts_mut(dest, sz as usize) };

            match source.read(foff, dest) {
                Ok(()) => info!("loaded {}", idx),
                Err(_) => {
                    error!("Failed reading");
                    return Err(());
                }
            }
        } else {
            info!("skipping noload {} 0x{:x}", idx, ph.p_paddr());
        }
    }

    let entry = loader.e_entry();
    info!("Entry address 0x{:x}", entry);
    Ok(entry)
}

const CMD_READ: u8 = 0x0B;
const CMD_ENABLE_RESET: u8 = 0x66;
const CMD_RESET: u8 = 0x99;
const CMD_READ_SR: u8 = 0x05;

/// Implementation of access to flash chip.
/// Chip commands are hardcoded as it depends on used chip.
pub struct FlashMemory<I: Instance> {
    xspi: Xspi<'static, I, Blocking>,
}

impl<I: Instance> FlashMemory<I> {
    pub async fn new(xspi: Xspi<'static, I, Blocking>) -> Self {
        let mut memory = Self { xspi };
        memory.reset_memory().await;
        memory
    }

    async fn exec_command(&mut self, cmd: u8) {
        let transaction = TransferConfig {
            iwidth: XspiWidth::SING,
            adwidth: XspiWidth::NONE,
            // adsize: AddressSize::_24bit,
            dwidth: XspiWidth::NONE,
            instruction: Some(cmd as u32),
            address: None,
            dummy: DummyCycles::_0,
            ..Default::default()
        };
        self.xspi.blocking_command(&transaction).unwrap();
    }

    pub async fn reset_memory(&mut self) {
        self.exec_command(CMD_ENABLE_RESET).await;
        self.exec_command(CMD_RESET).await;
        self.wait_write_finish();
    }

    pub fn read_memory(&mut self, addr: u32, buffer: &mut [u8]) {
        let transaction = TransferConfig {
            iwidth: XspiWidth::SING,
            adwidth: XspiWidth::SING,
            adsize: AddressSize::_24bit,
            dwidth: XspiWidth::SING,
            instruction: Some(CMD_READ as u32),
            dummy: DummyCycles::_8,
            address: Some(addr),
            ..Default::default()
        };
        self.xspi.blocking_read(buffer, transaction).unwrap();
    }

    fn wait_write_finish(&mut self) {
        while (self.read_sr() & 0x01) != 0 {}
    }

    fn read_register(&mut self, cmd: u8) -> u8 {
        let mut buffer = [0; 1];
        let transaction: TransferConfig = TransferConfig {
            iwidth: XspiWidth::SING,
            isize: AddressSize::_8bit,
            adwidth: XspiWidth::NONE,
            adsize: AddressSize::_24bit,
            dwidth: XspiWidth::SING,
            instruction: Some(cmd as u32),
            address: None,
            dummy: DummyCycles::_0,
            ..Default::default()
        };
        self.xspi.blocking_read(&mut buffer, transaction).unwrap();
        buffer[0]
    }

    pub fn read_sr(&mut self) -> u8 {
        self.read_register(CMD_READ_SR)
    }
}

// neotron_loader only passes const references, so wrap it in RefCell
struct FlashCell<I: Instance> {
    inner: RefCell<FlashMemory<I>>,
}

impl<I: Instance> neotron_loader::Source for &FlashCell<I> {
    type Error = ();

    fn read(&self, offset: u32, buffer: &mut [u8]) -> Result<(), ()> {
        let Some(end) = (offset as usize).checked_add(buffer.len()) else {
            error!("Bad read {:#x} len {:#x}", offset, buffer.len());
            return Err(());
        };

        if end > FLASH_SIZE {
            error!("Bad read {:#x} len {:#x}", offset, buffer.len());
            return Err(());
        }

        self.inner.borrow_mut().read_memory(offset, buffer);
        Ok(())
    }
}
