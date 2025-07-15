MEMORY
{
    FLASH : ORIGIN = 0x08000000, LENGTH =  0x20000
    EXT_FLASH : ORIGIN = 0x70000000, LENGTH = 0x2000000

    /* text/rodata in ITCM. Note that it is not accesible by peripherals */
    /* ITCM/SRAM1 split is set to non-default 192/0. */
    ITCM  : ORIGIN = 0x00000000, LENGTH =  192K

    /* Note that SRAM1 ORIGIN varies based on ITCM_AXI_SHARED */
    /* SRAM1   : ORIGIN = 0x24000000, LENGTH =  0K */

    /* DTCM/SRAM3 split is set to 128/64 */
    RAM  : ORIGIN = 0x20000000, LENGTH =  128K
    /* SRAM3 can be used for DMA */
    SRAM3 : ORIGIN = 0x24040000, LENGTH =  64K

    /* non-ECC. Used by bootloader. */
    SRAM2 : ORIGIN = 0x24020000, LENGTH =  128K
}
