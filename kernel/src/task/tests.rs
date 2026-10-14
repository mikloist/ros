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
    assert_eq!(s[sp + 8], 0xFFFF_FFFD, "EXC_RETURN: thread mode, PSP, no FP frame");
}

#[test]
fn hardware_frame_returns_into_entry() {
    let mut s = [0xAAAA_AAAA; 64];
    let sp = init_stack(&mut s, ENTRY, EXIT);
    let hw = &s[sp + 9..];
    assert_eq!(&hw[..5], &[0; 5], "r0-r3, r12");
    assert_eq!(hw[5], EXIT, "lr: returning from entry lands in the exit trap");
    assert_eq!(hw[6], ENTRY & !1, "pc: entry with Thumb bit cleared");
    assert_eq!(hw[7], 0x0100_0000, "xPSR: only the Thumb bit");
}


#[test]
#[should_panic]
fn rejects_stack_smaller_than_one_frame() {
    let mut s = [0u32; 16];
    init_stack(&mut s, ENTRY, EXIT);
}
