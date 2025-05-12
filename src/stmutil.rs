// SPDX-License-Identifier: GPL-3.0-only
/*
 * Copyright (c) 2025 Code Construct
 */

//! Helpers for stm32h7s3 hardware

pub fn device_id() -> [u8; 12] {
    let mut devid = [0u8; 12];
    /* Must read as u32 or u16. u8 is a BusFault */
    let src = (0x08FF_F800_usize) as *mut u32;

    for (i, dest) in devid.chunks_mut(size_of::<u32>()).enumerate() {
        let b = unsafe { src.add(i).read_volatile() };
        dest.copy_from_slice(&b.to_ne_bytes());
    }
    devid
}
