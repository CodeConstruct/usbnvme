MEMORY
{
    FLASH : ORIGIN = 0x08000000, LENGTH =  0x20000

    /* text/rodata in ITCM. Note that it is not accesible by peripherals */
    /* ITCM/SRAM1 split is set to non-default 128/64. */
    ITCM  : ORIGIN = 0x00000000, LENGTH =  128K
    SRAM1   : ORIGIN = 0x24000000, LENGTH =  64K

    /* DTCM/SRAM3 split is set to non-default 192/0. */
    /* Use DTCM for RAM. Note that it is not accessible by peripherals */
    RAM  : ORIGIN = 0x20000000, LENGTH =  192K
    SRAM3 : ORIGIN = 0x24040000, LENGTH =  0K

    /* non-ECC. Used by bootloader. */
    SRAM2 : ORIGIN = 0x24020000, LENGTH =  128K
}
