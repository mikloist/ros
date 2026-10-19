//! Host tests for the initial task frame. Run: `cargo ktest`.
use super::*;

const ENTRY: u32 = 0x1000_0101; // Thumb fn pointers have bit 0 set
const EXIT: u32 = 0x1000_0201;

#[test]
fn frame_sits_at_top_of_stack() {
    let mut s = [0xAAAA_AAAA; 64];
    let sp = init_stack(&mut s, ENTRY, EXIT);
    assert_eq!(sp, 64 - FRAME_WORDS);
    assert_eq!(s[sp - 1], 0xAAAA_AAAA, "wrote below the frame");
}

#[test]
fn software_frame_is_zeroed_regs_then_exc_return() {
    let mut s = [0xAAAA_AAAA; 64];
    let sp = init_stack(&mut s, ENTRY, EXIT);
    assert_eq!(&s[sp..sp + 8], &[0; 8], "r4-r11");
    assert_eq!(
        s[sp + 8],
        0xFFFF_FFFD,
        "EXC_RETURN: thread mode, PSP, no FP frame"
    );
}

#[test]
fn hardware_frame_returns_into_entry() {
    let mut s = [0xAAAA_AAAA; 64];
    let sp = init_stack(&mut s, ENTRY, EXIT);
    let hw = &s[sp + 9..];
    assert_eq!(&hw[..5], &[0; 5], "r0-r3, r12");
    assert_eq!(
        hw[5], EXIT,
        "lr: returning from entry lands in the exit trap"
    );
    assert_eq!(hw[6], ENTRY & !1, "pc: entry with Thumb bit cleared");
    assert_eq!(hw[7], 0x0100_0000, "xPSR: only the Thumb bit");
}
#[test]
fn task_limit_is_stack_bottom_and_sp_is_frame_start() {
    static STACK: Stack<64> = Stack::new();
    fn never() -> ! {
        loop {}
    }
    let t = Task::new("t", 1, never, &STACK);
    let base = STACK.stack.get() as *mut u32;
    // PSPLIM is the lowest legal address: the stack grows down toward it.
    assert_eq!(
        t.stack_limit, base,
        "stack_limit must be the bottom, not the top"
    );
    assert_eq!(
        t.sp,
        base.wrapping_add(64 - FRAME_WORDS),
        "sp must point at the frame start"
    );
}

fn never() -> ! {
    loop {}
}

#[test]
#[should_panic(expected = "task priority must be 1..=7")]
fn new_rejects_idle_priority() {
    static STACK: Stack<64> = Stack::new();
    Task::new("t", 0, never, &STACK);
}

#[test]
#[should_panic(expected = "task priority must be 1..=7")]
fn new_rejects_priority_above_7() {
    static STACK: Stack<64> = Stack::new();
    Task::new("t", 8, never, &STACK);
}

#[test]
fn idle_gets_priority_zero() {
    static STACK: Stack<64> = Stack::new();
    assert_eq!(Task::idle(never, &STACK).current_priority, 0);
}

#[test]
#[should_panic(expected = "Stack has to be bigger")]
fn rejects_stack_smaller_than_one_frame() {
    // Must hit the explicit size check, not an arithmetic-overflow panic.
    let mut s = [0u32; FRAME_WORDS - 1];
    init_stack(&mut s, ENTRY, EXIT);
}

#[test]
fn accepts_stack_of_exactly_one_frame() {
    let mut s = [0xAAAA_AAAA; FRAME_WORDS];
    assert_eq!(init_stack(&mut s, ENTRY, EXIT), 0);
}

// ---- M6: peripheral grants and stack bounds ----

fn task_with_stack(lo: usize, words: usize) -> Task {
    let mut t = Task::for_test(1);
    t.stack_limit = lo as *mut u32;
    t.stack_end = (lo + 4 * words) as *mut u32;
    t
}

#[test]
fn granted_peripherals_fill_the_region_slots_in_order() {
    let t = Task::for_test(1).with_peripheral(0xD000_0000, 0x100).with_peripheral(0x400B_0000, 0x40);
    assert_eq!(t.peripherals[0], Region::device(0xD000_0000, 0x100));
    assert_eq!(t.peripherals[1], Region::device(0x400B_0000, 0x40));
    assert_eq!(t.peripherals[2], Region::DISABLED);
}

#[test]
#[should_panic(expected = "at most 5")]
fn a_sixth_peripheral_is_rejected() {
    let mut t = Task::for_test(1);
    for i in 0..6 {
        t = t.with_peripheral(0x4000_0000 + i * 0x1000, 0x100);
    }
}

#[test]
fn stack_contains_checks_both_ends() {
    let t = task_with_stack(0x2001_0000, 64); // 0x2001_0000..0x2001_0100
    assert!(t.stack_contains(0x2001_0000, 8), "first bytes");
    assert!(t.stack_contains(0x2001_00F8, 8), "last bytes");
    assert!(!t.stack_contains(0x2001_00FC, 8), "runs past the end");
    assert!(!t.stack_contains(0x2000_FFFC, 8), "starts below the stack");
    assert!(!t.stack_contains(usize::MAX - 4, 8), "wraps around the address space");
}
