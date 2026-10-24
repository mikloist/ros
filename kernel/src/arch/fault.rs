//! MemManage, BusFault and UsageFault (incl. stack overflow): a fault in a
//! task kills only that task; a fault anywhere else halts the system.

use cortex_m::register::{basepri, psp, psplim};

use super::with_sched;

/// Configurable Fault Status Register: which fault, and why.
const CFSR: *mut u32 = 0xE000_ED28 as *mut u32;
/// Interrupt Control and State Register.
const ICSR: *const u32 = 0xE000_ED04 as *const u32;
/// ICSR.RETTOBASE: this exception is the only active one, so it interrupted
/// thread mode (a task), not another handler.
const RETTOBASE: u32 = 1 << 11;

// The names must match cortex-m-rt's vector table exactly.
#[unsafe(no_mangle)]
extern "C" fn MemoryManagement() {
    fault("MemManage");
}

#[unsafe(no_mangle)]
extern "C" fn BusFault() {
    fault("BusFault");
}

#[unsafe(no_mangle)]
extern "C" fn UsageFault() {
    fault("UsageFault");
}

fn fault(kind: &str) {
    // SAFETY: SCB registers, only touched by fault handlers.
    let cfsr = unsafe { CFSR.read_volatile() };
    let from_thread = unsafe { ICSR.read_volatile() } & RETTOBASE != 0;
    let task = with_sched(|s| s.current());
    // A fault in a handler, in `main`, or in the idle task (part of the
    // kernel) leaves nothing safe to run: stop the system.
    if !from_thread || task.is_null() || unsafe { (*task).original_priority } == 0 {
        panic!("{} in kernel context, CFSR {:08x}", kind, cfsr);
    }
    unsafe {
        // The task may have died inside its critical section, leaving BASEPRI
        // raised; that would keep SysTick and PendSV masked forever.
        basepri::write(0);
        defmt::error!("task '{}' killed: {} (CFSR {=u32:08x})", (*task).name, kind, cfsr);
        // Write-1-to-clear, so the next fault reports only its own bits.
        CFSR.write_volatile(cfsr);
    }
    with_sched(|s| s.kill_current());
    // Returning from this handler, the core unstacks a frame from PSP
    // *before* it takes the pending PendSV, with the dead task's
    // (unprivileged) rights. So PSP must point at a frame the task itself may
    // read: inside its own stack. Start `PARK_ROOM` words above the limit, so
    // PendSV's register saves below PSP stay in this stack even after an
    // overflow, and fill in a clean frame whose pc is a harmless loop in
    // flash (PendSV switches away before it ever runs).
    unsafe {
        let frame = (*task).stack_limit.add(PARK_ROOM);
        frame.write_bytes(0, PARK_FRAME);
        frame.add(6).write(park as fn() -> ! as usize as u32 & !1); // pc
        frame.add(7).write(0x0100_0000); // xPSR: Thumb bit
        psplim::write((*task).stack_limit as u32);
        psp::write(frame as u32);
    }
    cortex_m::peripheral::SCB::set_pendsv();
}

/// Words left below the parked frame for PendSV's saves (r4-r11, EXC_RETURN,
/// and s16-s31 if the task used the FPU: at most 25).
const PARK_ROOM: usize = 32;
/// Words of the parked frame: 8, or 26 with an FPU frame. Zero them all.
const PARK_FRAME: usize = 26;

/// Where a killed task would "resume" if PendSV did not switch it out first.
fn park() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}
