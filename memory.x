MEMORY
{
    FLASH : ORIGIN = 0x08000000, LENGTH =  0x20000
    EXT_FLASH : ORIGIN = 0x70000000, LENGTH = 0x2000000

    /* text/rodata in ITCM. Note that it is not accesible by peripherals */
    /* ITCM/SRAM1 split is set to non-default 128/64. */
    ITCM  : ORIGIN = 0x00000000, LENGTH =  128K
    SRAM1   : ORIGIN = 0x24000000, LENGTH =  64K

    /* DTCM/SRAM3 split is set to 64/128 */
    /* Use SRAM3 for RAM. */
    DTCM  : ORIGIN = 0x20000000, LENGTH =  64K
    RAM : ORIGIN = 0x24040000, LENGTH =  128K

    /* non-ECC. Used by bootloader. */
    SRAM2 : ORIGIN = 0x24020000, LENGTH =  128K
}
