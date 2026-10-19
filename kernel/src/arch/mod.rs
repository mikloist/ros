//! Cortex-M33 glue: PendSV context switch, SysTick, critical sections, and the
//! public task API. All scheduling decisions live in `sched.rs`.

mod fault;
pub(crate) mod svc;

use core::arch::global_asm;

use cortex_m::peripheral::scb::{Exception, SystemHandler};
use cortex_m::peripheral::syst::SystClkSource;

use core::time::Duration;

use crate::arch::svc::{Status, Sys, in_handler, split, svc};
use crate::sched::{Instant, Sched, timeout_to_ticks};
use crate::task::{Stack, Task};

/// The one scheduler. Only touch it through `with_sched`.
static mut SCHED: Sched = Sched::new();
/// The kernel's idle task, created by `start`.
static mut IDLE: Option<Task> = None;
static IDLE_STACK: Stack<128> = Stack::new();
/// PSP target for the very first PendSV, which "saves" a non-task's registers.
static mut DUMMY: [u32; 32] = [0; 32];

/// Runs `f` on the scheduler with interrupts masked (PRIMASK), so SysTick
/// cannot touch it halfway through. Every access to `SCHED` goes through here.
pub(crate) fn with_sched<R>(f: impl FnOnce(&mut Sched) -> R) -> R {
    cortex_m::interrupt::free(|_| {
        let s = unsafe { &mut *(&raw mut SCHED) };
        return f(s);
    })
}

/// Runs when nothing else is ready: sleep the core until the next interrupt.
fn idle() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}

/// Makes every task (and the idle task) ready, sets PendSV to the lowest
/// priority and SysTick above it, starts SysTick at 1 kHz from `sys_freq`,
/// points PSP at `DUMMY`, and pends the first switch. Never returns.
pub fn start(tasks: &mut [Task], sys_freq: u32) -> ! {
    assert!(!tasks.is_empty(), "start needs at least one task");
    // One base pointer for every task: `tasks` is never borrowed again, so the
    // pointers the scheduler keeps stay valid (and `start` never returns).
    let base = tasks.as_mut_ptr();
    let n = tasks.len();
    // SAFETY: interrupts are not running kernel code yet (SysTick is off, no
    // PendSV pended); IDLE is written once here and only read via its pointer.
    let idle_task = unsafe { (*&raw mut IDLE).insert(Task::idle(idle, &IDLE_STACK)) as *mut Task };
    with_sched(|s| unsafe {
        for i in 0..n {
            s.make_ready(base.add(i));
        }
        s.make_ready(idle_task);
    });

    // SAFETY: the HAL owns the device peripherals, not these core ones; the
    // kernel is the only user of SCB priorities, SYST and PSP.
    unsafe {
        let mut p = cortex_m::Peripherals::steal();
        // PendSV lowest so a switch never interrupts a handler; SysTick above it.
        p.SCB.set_priority(SystemHandler::PendSV, 0xFF);
        p.SCB.set_priority(SystemHandler::SysTick, 0x80);
        // Task faults get their own handlers (kill the task); disabled, they
        // would all escalate to HardFault and stop the system.
        p.SCB.enable(Exception::MemoryManagement);
        p.SCB.enable(Exception::BusFault);
        p.SCB.enable(Exception::UsageFault);
        // SysTick counts core cycles: reload N-1 for an N-cycle (1 ms) period.
        p.SYST.set_clock_source(SystClkSource::Core);
        p.SYST.set_reload(sys_freq / 1000 - 1);
        p.SYST.clear_current();
        p.SYST.enable_interrupt();
        p.SYST.enable_counter();
        cortex_m::register::psp::write((&raw mut DUMMY as *mut u32).add(32) as u32);
    }
    yield_now();
    unreachable!()
}

/// Hands the CPU to the next task (from an interrupt handler: once it returns).
pub fn yield_now() {
    if in_handler() {
        cortex_m::peripheral::SCB::set_pendsv();
    } else {
        svc::<{ Sys::Yield as u8 }>(0, 0, 0, 0);
    }
}

/// Time since `start` (1 ms ticks). Callable from interrupt handlers.
pub fn now() -> Instant {
    if in_handler() {
        // u64 is two words on a 32-bit core: read it with SysTick masked.
        return with_sched(|s| s.now());
    }
    let (lo, hi) = svc::<{ Sys::Now as u8 }>(0, 0, 0, 0);
    Instant::from_ticks((hi as u64) << 32 | lo as u64)
}

/// Sleeps until `deadline`; returns at once if it is now or in the past.
pub fn sleep_until(deadline: Instant) {
    let (lo, hi) = split(deadline.ticks());
    svc::<{ Sys::SleepUntil as u8 }>(lo, hi, 0, 0);
}

/// Sleeps for `d`, rounded up to whole ticks. The kernel adds `d` to the
/// current tick itself, so a tick between asking and sleeping cannot shorten it.
pub fn sleep(d: Duration) {
    let (lo, hi) = split(timeout_to_ticks(Some(d)));
    svc::<{ Sys::Sleep as u8 }>(lo, hi, 0, 0);
}

/// SysTick exception (1 kHz): advance time; pend PendSV if `tick` says so.
/// The name must be exactly `SysTick` for cortex-m-rt's vector table.
#[unsafe(no_mangle)]
extern "C" fn SysTick() {
    if with_sched(|s| s.tick()) {
        cortex_m::peripheral::SCB::set_pendsv();
    }
}

/// Called from PendSV with the outgoing task's saved stack pointer in r0.
/// Inside `with_sched`: store `sp` into the current task (if any), let
/// `Sched::switch` pick the next one, load its `stack_limit` into PSPLIM, and
/// return its saved stack pointer.
#[unsafe(no_mangle)]
extern "C" fn kernel_switch(sp: *mut u32) -> *mut u32 {
    with_sched(|s| {
        let current = s.current();
        if !current.is_null() {
            unsafe {
                (*current).sp = sp;
            }
        }
        let next = s.switch();
        unsafe {
            // A task resuming from a blocking syscall gets its result in r0.
            let f = (*next).syscall_frame;
            if !f.is_null() {
                *f = if (*next).timed_out {
                    Status::Timeout
                } else {
                    Status::Ok
                } as u32;
                (*next).syscall_frame = core::ptr::null_mut();
            }
            // Thread-mode privilege for the incoming task. Change only nPRIV
            // (CONTROL also holds SPSEL and FPCA); PendSV's `bx lr` applies it.
            let mut c = cortex_m::register::control::read();
            c.set_npriv(if (*next).privileged {
                cortex_m::register::control::Npriv::Privileged
            } else {
                cortex_m::register::control::Npriv::Unprivileged
            });

            cortex_m::register::control::write(c);
            // PSPLIM takes the address as a plain u32; `next` is valid (just picked by `switch`).
            cortex_m::register::psplim::write((*next).stack_limit as u32);
            return (*next).sp;
        }
    })
}

// PendSV: the context switch. On entry the hardware has already pushed the
// outgoing task's r0-r3, r12, lr, pc, xPSR (+ s0-s15, FPSCR if it used the
// FPU) onto its PSP stack, and lr holds EXC_RETURN. We save the rest, let
// `kernel_switch` pick the next task, and restore that task's registers in the
// exact mirror order. Layout must match `init_stack` (see task.rs).
global_asm!(
    ".section .text.PendSV,\"ax\",%progbits",
    ".global PendSV", // cortex-m-rt's vector table links to this name
    ".type PendSV,%function",
    ".thumb_func", // Thumb code: the vector entry gets bit 0 set
    "PendSV:",
    // 1. r0 = outgoing task's stack pointer.
    "  mrs r0, psp",
    // 2. EXC_RETURN bit 4 == 0 means the task used the FPU: save s16-s31
    //    (callee-saved FP regs; hardware only saves s0-s15). `vstmdb r0!`
    //    stores 16 words below r0 and moves r0 down past them.
    "  tst lr, #0x10",
    "  it eq",
    "  vstmdbeq r0!, {{s16-s31}}",
    // 3. Save the callee-saved core regs and EXC_RETURN (this task's own way
    //    back). r4 lands at the lowest address, lr at the highest.
    "  stmdb r0!, {{r4-r11, lr}}",
    // 4. Rust picks the next task: r0 in (old sp), r0 out (new sp).
    //    `bl` clobbers lr; fine, we saved it above and pop the new one below.
    "  bl kernel_switch",
    // 5. Restore the incoming task's core regs and its EXC_RETURN.
    "  ldmia r0!, {{r4-r11, lr}}",
    // 6. Mirror of step 2, using the *incoming* task's EXC_RETURN.
    "  tst lr, #0x10",
    "  it eq",
    "  vldmiaeq r0!, {{s16-s31}}",
    // 7. r0 now points at the hardware frame. Hand it to PSP and return:
    //    `bx` with an EXC_RETURN value makes the hardware pop that frame and
    //    resume the task at its stacked pc.
    "  msr psp, r0",
    "  bx lr",
);
