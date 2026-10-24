//! Programs the MPU (ARMv8-M PMSA, 8 regions). Fixed regions are set once in
//! `init`; each task's stack and peripheral regions are loaded on every switch.
//!
//! | #   | region              | set by |
//! |-----|---------------------|--------|
//! | 0   | flash (code)        | `init` |
//! | 1   | own stack           | `load` |
//! | 2   | shared RAM          | `init` |
//! | 3-7 | granted peripherals | `load` |

// ---- MPU registers (System Control Space, privileged only) ----

use crate::{
    Task,
    region::{MAIR0, Region},
};

/// MPU_CTRL: enable bits.
const CTRL: *mut u32 = 0xE000_ED94 as *mut u32;
/// MPU_RNR: selects which region RBAR/RLAR refer to.
const RNR: *mut u32 = 0xE000_ED98 as *mut u32;
/// MPU_RBAR: base address and access bits of the selected region.
const RBAR: *mut u32 = 0xE000_ED9C as *mut u32;
/// MPU_RLAR: limit address, memory attribute index and enable bit.
const RLAR: *mut u32 = 0xE000_EDA0 as *mut u32;
/// MPU_MAIR0: memory types for attribute indices 0..=3.
const MAIR0_REG: *mut u32 = 0xE000_EDC0 as *mut u32;

/// MPU_CTRL.ENABLE: turn the MPU on.
const ENABLE: u32 = 1 << 0;
/// MPU_CTRL.PRIVDEFENA: privileged code sees the default memory map where no
/// region applies (kernel and handlers reach `.kobj`, every stack, every
/// peripheral). Unprivileged code faults there.
const PRIVDEFENA: u32 = 1 << 2;

// ---- Region numbers ----

/// All of flash: read-only, executable.
const FLASH: u32 = 0;
/// The running task's stack.
const STACK: u32 = 1;
/// Shared RAM (`.data`, `.bss`, `.uninit`).
const SHARED: u32 = 2;
/// First of the 5 granted-peripheral regions (3..=7).
const FIRST_PERIPHERAL: u32 = 3;

// ---- Linker symbols (app/memory.x); only their addresses matter ----

unsafe extern "C" {
    static __flash_start: u8;
    static __flash_end: u8;
    /// Start of shared RAM.
    static __sdata: u8;
    /// End of shared RAM = start of `.kobj`.
    static __skobj: u8;
}

unsafe fn set(region_id: u32, region: Region) {
    unsafe {
        RNR.write_volatile(region_id);
        RLAR.write_volatile(0);
        RBAR.write_volatile(region.rbar);
        RLAR.write_volatile(region.rlar);
    }
}

fn addr(sym: &u8) -> usize {
    sym as *const u8 as usize
}

pub(crate) unsafe fn init() {
    unsafe {
        MAIR0_REG.write_volatile(MAIR0);

        let flash_region = Region::code(
            addr(&__flash_start),
            addr(&__flash_end) - addr(&__flash_start),
        );

        let data_region = Region::data(addr(&__sdata), addr(&__skobj) - addr(&__sdata));

        set(FLASH, flash_region);
        set(SHARED, data_region);

        CTRL.write_volatile(ENABLE | PRIVDEFENA);

        cortex_m::asm::isb();
    }
}

pub(crate) unsafe fn load(t: &Task) {
    let task_data_region =
        Region::data(
            t.stack_limit as usize,
            unsafe { t.stack_end.byte_offset_from(t.stack_limit) } as usize,
        );
    unsafe {
        set(STACK, task_data_region);

        for (i, &p) in t.peripherals.iter().enumerate() {
            set(FIRST_PERIPHERAL + i as u32, p);
        }
        cortex_m::asm::dsb();
    }
}
