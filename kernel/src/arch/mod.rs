//! Cortex-M33 context switching (M1: cooperative round-robin).

use core::arch::{asm, global_asm};
use core::ptr::null_mut;

use crate::task::Task;

static mut CURRENT: *mut Task = null_mut();
static mut FIRST: *mut Task = null_mut();
static mut DUMMY: [u32; 32] = [0; 32];

pub fn start(tasks: &mut [Task]) -> ! {
    assert!(!tasks.is_empty(), "start needs at least one task");
    for i in 1..tasks.len() {
        tasks[i - 1].next = &mut tasks[i];
    }
    tasks.last_mut().unwrap().next = &mut tasks[0];
    unsafe {
        FIRST = tasks.as_mut_ptr();
        cortex_m::Peripherals::steal()
            .SCB
            .set_priority(cortex_m::peripheral::scb::SystemHandler::PendSV, 0xFF);
        cortex_m::register::psp::write((&raw mut DUMMY as *mut u32).add(32) as u32);
    }
    yield_now();
    unreachable!()
}

pub fn yield_now() {
    cortex_m::peripheral::SCB::set_pendsv();
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

#[unsafe(no_mangle)]
extern "C" fn kernel_switch(sp: *mut u32) -> *mut u32 {
    // SAFETY: runs only inside PendSV (lowest priority, never re-entered), so
    // nothing else touches CURRENT/FIRST or the task records concurrently.
    unsafe {
        let mut next = null_mut();
        if CURRENT == null_mut() {
            next = FIRST;
        } else {
            (*CURRENT).sp = sp;
            next = (*CURRENT).next;
        }
        CURRENT = next;
        asm!("msr psplim, {}", in(reg) (*next).stack_limit);
        return (*next).sp;
    }
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
