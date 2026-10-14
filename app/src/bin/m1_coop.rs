//! M1: two tasks alternate via cooperative `yield_now`; the context switch
//! preserves callee-saved FP registers (s16-s31) that only PendSV can save.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::{AtomicU32, Ordering::SeqCst};
use kernel::{Stack, Task};

const ROUNDS: u32 = 5;
static TURN: AtomicU32 = AtomicU32::new(0);
static STACK_A: Stack<512> = Stack::new();
static STACK_B: Stack<512> = Stack::new();

/// Fills s16-s31 with `seed, seed+1, ..`, pends PendSV *inside the same asm
/// block* (so the compiler cannot spill them for us), then checks they still
/// hold the pattern. The other task does the same with a different seed, so a
/// switch that skips s16-s31 hands us its values and this returns false.
fn fp_regs_survive_switch(seed: u32) -> bool {
    let bad: u32;
    unsafe {
        asm!(
        "mov {bad}, #0",
        "vmov s16, {s}",
        "add {s}, {s}, #1",
        "vmov s17, {s}",
        "add {s}, {s}, #1",
        "vmov s18, {s}",
        "add {s}, {s}, #1",
        "vmov s19, {s}",
        "add {s}, {s}, #1",
        "vmov s20, {s}",
        "add {s}, {s}, #1",
        "vmov s21, {s}",
        "add {s}, {s}, #1",
        "vmov s22, {s}",
        "add {s}, {s}, #1",
        "vmov s23, {s}",
        "add {s}, {s}, #1",
        "vmov s24, {s}",
        "add {s}, {s}, #1",
        "vmov s25, {s}",
        "add {s}, {s}, #1",
        "vmov s26, {s}",
        "add {s}, {s}, #1",
        "vmov s27, {s}",
        "add {s}, {s}, #1",
        "vmov s28, {s}",
        "add {s}, {s}, #1",
        "vmov s29, {s}",
        "add {s}, {s}, #1",
        "vmov s30, {s}",
        "add {s}, {s}, #1",
        "vmov s31, {s}",
        "add {s}, {s}, #1",
        "movw {t}, #0xED04", // ICSR
        "movt {t}, #0xE000",
        "mov {u}, #0x10000000", // PENDSVSET
        "str {u}, [{t}]",
        "dsb",
        "isb", // PendSV taken here; other task runs
        "sub {s}, {s}, #16",
        "vmov {u}, s16",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s17",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s18",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s19",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s20",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s21",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s22",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s23",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s24",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s25",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s26",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s27",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s28",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s29",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s30",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        "vmov {u}, s31",
        "eor {u}, {u}, {s}",
        "orr {bad}, {bad}, {u}",
        "add {s}, {s}, #1",
        s = inout(reg) seed => _,
        t = out(reg) _,
        u = out(reg) _,
        bad = out(reg) bad,
        out("s16") _, out("s17") _, out("s18") _, out("s19") _, out("s20") _, out("s21") _, out("s22") _, out("s23") _, out("s24") _, out("s25") _, out("s26") _, out("s27") _, out("s28") _, out("s29") _, out("s30") _, out("s31") _,
        );
    }
    bad == 0
}

fn task_a() -> ! {
    for i in 0..ROUNDS {
        assert_eq!(TURN.load(SeqCst), 2 * i, "a ran out of turn");
        defmt::info!("a round {}", i);
        TURN.fetch_add(1, SeqCst);
        assert!(fp_regs_survive_switch(0x1000), "a: s16-s31 not preserved");
    }
    loop {
        kernel::yield_now();
    }
}

fn task_b() -> ! {
    for i in 0..ROUNDS {
        assert_eq!(TURN.load(SeqCst), 2 * i + 1, "b ran out of turn");
        defmt::info!("b round {}", i);
        TURN.fetch_add(1, SeqCst);
        assert!(fp_regs_survive_switch(0x2000), "b: s16-s31 not preserved");
    }
    kernel::yield_now(); // a gets one more turn: yield_now itself must switch
    assert_eq!(TURN.load(SeqCst), 2 * ROUNDS);
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    app::init();
    let mut tasks = [Task::new("a", task_a, &STACK_A), Task::new("b", task_b, &STACK_B)];
    kernel::start(&mut tasks)
}
