//! MemManage, BusFault and UsageFault (incl. stack overflow): a fault in a
//! task kills only that task; a fault anywhere else halts the system.

use cortex_m::register::{basepri, psp, psplim};

use super::{DUMMY, with_sched};

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
    // PendSV (tail-chained right after this handler) saves the dead task's
    // registers below PSP. After a stack overflow PSP sits at the limit, so
    // those stores would fault again or land in memory below the stack.
    // Point PSP at scratch space instead; PSPLIM goes to 0 first so the new
    // PSP is never below the limit.
    unsafe {
        psplim::write(0);
        psp::write((&raw mut DUMMY as *mut u32).add(32) as u32);
    }
    cortex_m::peripheral::SCB::set_pendsv();
}
