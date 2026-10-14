# M1: Cooperative Context Switch — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Ownership:** Tasks 1 and 4 are Claude's (plumbing, tests, review). Tasks 2 and 3 are the **user's** (kernel core): an agent MUST NOT write `kernel/src/task.rs` or `kernel/src/arch/`; it may answer questions and review.

**Goal:** Two tasks take turns on core 0 via `yield_now()`; PendSV saves/restores every register (integer and FP), and PSPLIM catches stack overflow.

**Architecture:** `task.rs` builds each task's first stack frame so the very first switch "returns" into the task. `arch/mod.rs` owns PendSV (asm), the current-task pointer and a round-robin ring through `Task::next`. No priorities, no tick: those are M2.

**Tech Stack:** M0 workspace; `cortex-m` 0.7 (Arm-only dependency of `kernel`); `core::arch::{asm, global_asm}`.

**Spec:** `docs/superpowers/specs/2026-10-14-rtos-kernel-design.md` (section 2, milestone M1)

## Global Constraints

- Everything from the M0 plan's Global Constraints still holds.
- `kernel` stays host-testable: `arch` and anything using `cortex-m` is behind `#[cfg(target_arch = "arm")]`.
- No heap. Stacks are `static`s of `Stack<N>` (N in 32-bit words, 32-byte aligned).
- Interfaces below are fixed: the tests and demo binaries compile against them.
- Board runs: `timeout -s INT 120`; exit 124 = slow probe-rs attach, rerun.

## Fixed interfaces

```rust
// kernel/src/task.rs
pub struct Stack<const N: usize> { /* .. */ }      // #[repr(align(32))], Sync, N words
impl<const N: usize> Stack<N> { pub const fn new() -> Self }

pub struct Task { /* sp, stack_limit, next, name */ }
impl Task {
    pub fn new<const N: usize>(name: &'static str, entry: fn() -> !, stack: &'static Stack<N>) -> Task;
}
pub(crate) const FRAME_WORDS: usize = 17;
/// Writes the initial frame at the top of `stack`; returns the index the task's saved SP points at.
pub(crate) fn init_stack(stack: &mut [u32], entry: u32, on_exit: u32) -> usize;

// kernel/src/arch/mod.rs  (Arm only)
pub fn start(tasks: &mut [Task]) -> !;
pub fn yield_now();
```

Saved-stack layout (lowest address = saved `sp`), the contract between `init_stack` and PendSV:

| Word offset from `sp` | Contents | Pushed by |
|---|---|---|
| 0–7 | r4–r11 | PendSV (`stmdb`) |
| 8 | EXC_RETURN | PendSV |
| 9–24 *(only if EXC_RETURN bit 4 == 0)* | s16–s31 | PendSV (`vstmdb`) |
| next 8 | r0, r1, r2, r3, r12, lr, pc, xPSR | hardware |
| *(+ s0–s15, FPSCR if FP frame)* | | hardware (lazy) |

A fresh task has no FP frame, so its 17 words are offsets 0–8 plus the 8 hardware words.

---

### Task 1 (Claude): M1 harness — wiring, host tests, demo binaries

**Files:**
- Modify: `kernel/Cargo.toml`, `kernel/src/lib.rs`
- Create: `kernel/src/task/tests.rs`, `app/src/bin/m1_coop.rs`, `app/src/bin/m1_overflow.rs`

**Interfaces:**
- Produces: the fixed interfaces above as *expectations*; `cargo ktest` and the two binaries stay red until Tasks 2–3 land.

- [ ] **Step 1: Add the Arm-only dependency to `kernel/Cargo.toml`**

```toml
[package]
name = "kernel"
version = "0.1.0"
edition = "2024"

[dependencies]

[target.'cfg(target_arch = "arm")'.dependencies]
cortex-m = "0.7"
```

- [ ] **Step 2: Wire modules in `kernel/src/lib.rs`**

```rust
//! ros kernel. Pure logic is host-testable; `arch` is Cortex-M33 only.
#![cfg_attr(not(test), no_std)]

mod task;
pub use task::{Stack, Task};

#[cfg(target_arch = "arm")]
mod arch;
#[cfg(target_arch = "arm")]
pub use arch::{start, yield_now};
```

- [ ] **Step 3: Write `kernel/src/task/tests.rs`**

Hooked up by `#[cfg(test)] mod tests;` at the bottom of the user's `task.rs` (Task 2).

```rust
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
```

- [ ] **Step 4: Write `app/src/bin/m1_coop.rs`**

`fp_regs_survive_switch` is deliberately asm: a plain Rust `f32` test was verified (2026-10-14) to still pass with s16–s31 saving removed, because the compiler spills floats to the stack around calls.

```rust
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
```

- [ ] **Step 5: Write `app/src/bin/m1_overflow.rs`**

```rust
//! M1: a task that overflows its stack must fault (PSPLIM), not corrupt memory.
//! Expected: `probe-rs run` exits 1 with a stack-overflow UsageFault.
//! If this prints `TEST PASS`, PSPLIM is not being set.
#![no_std]
#![no_main]

use kernel::{Stack, Task};

static STACK_A: Stack<256> = Stack::new();

#[inline(never)]
fn recurse(n: u32) -> u32 {
    let buf = core::hint::black_box([n; 16]);
    if n == 0 { 0 } else { recurse(n - 1).wrapping_add(buf[3]) }
}

fn task_a() -> ! {
    defmt::info!("recursing past 1 KiB stack");
    core::hint::black_box(recurse(1000));
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    app::init();
    let mut tasks = [Task::new("a", task_a, &STACK_A)];
    kernel::start(&mut tasks)
}
```

- [ ] **Step 6: Commit the red harness**

```bash
git add kernel app/src/bin/m1_coop.rs app/src/bin/m1_overflow.rs
git commit -m "test(m1): context-switch harness (red until task/arch land)"
```

---

### Task 2 (User): task stacks and the initial frame — `kernel/src/task.rs`

**Goal:** `cargo ktest` → 4 passed.

**Concepts to read first**
- Armv8-M exception entry/return: what the hardware pushes, EXC_RETURN bits (Armv8-M ARM, DDI0553, "Exception entry" / "Exception return"; Cortex-M33 Devices Generic User Guide, "Exception model").
- AAPCS: r4–r11 and s16–s31 are callee-saved; r0–r3, r12, lr, s0–s15 are not. That split is why hardware saves one half and PendSV the other.

**Build**
1. `Stack<N>`: wrap `[u32; N]` in `UnsafeCell`, `#[repr(C, align(32))]`, `unsafe impl Sync`. `const fn new()` zero-fills.
2. `Task { sp: *mut u32, stack_limit: *mut u32, next: *mut Task, name: &'static str }` (fields `pub(crate)` except `name`).
3. `init_stack(stack, entry, on_exit) -> usize`: assert room for `FRAME_WORDS`, write the 17-word frame at the top per the layout table, return its start index.
4. `Task::new`: get `&mut [u32]` from the `UnsafeCell`, call `init_stack`, set `sp` = base + index, `stack_limit` = base (lowest address).
5. A private `fn task_exit() -> !` that panics ("task returned"), used as `on_exit`.
6. `#[cfg(test)] mod tests;` at the bottom.

**Hints**
- Values: EXC_RETURN `0xFFFF_FFFD` (Secure, thread mode, PSP, no FP frame); xPSR `0x0100_0000` (Thumb bit only).
- Thumb function addresses have bit 0 set. The stacked PC must have it **cleared**; the stacked LR keeps it.
- On host, `fn` pointers are 64-bit: `init_stack` takes `u32` so tests don't care. On target, `entry as usize as u32` is exact.
- `task_exit as usize` warns (`function_casts_as_integer`); cast through the fn type: `task_exit as fn() -> ! as usize as u32`.
- `Task::new` taking a `&'static Stack` and handing out `&mut` is only sound if each stack backs one task. Document that invariant; M6's MPU will enforce isolation.

**Verify:** `cargo ktest` → `test result: ok. 4 passed`. Then ping Claude for review.

---

### Task 3 (User): PendSV, start, yield — `kernel/src/arch/mod.rs`

**Goal:** `m1_coop` exits 0; `m1_overflow` exits 1 with a stack-overflow fault.

**Concepts to read first**
- PendSV as the "deferred switch" exception, why it runs at the lowest priority.
- MSP vs PSP; `mrs/msr psp`; PSPLIM (Armv8-M stack limit checking, UsageFault STKOF).
- Lazy FP stacking: EXC_RETURN bit 4 == 0 means the hardware frame is the extended FP frame and s16–s31 are live.
- Reading reference: FreeRTOS `portable/GCC/ARM_CM33_NTZ/non_secure/portasm.c` (`PendSV_Handler`). Read it, then write yours without copying.

**Build**
1. Kernel state: `static mut CURRENT: *mut Task` (null before start), first task pointer, a small dummy stack (32 words).
2. `start(tasks)`: link `tasks` into a ring via `next`; set PendSV priority to lowest (`0xFF`); point PSP at the top of the dummy stack; pend PendSV; never return.
3. `yield_now()`: pend PendSV.
4. `extern "C" fn kernel_switch(sp: *mut u32) -> *mut u32` (`#[unsafe(no_mangle)]`): if a task is current, store `sp` into it and advance to `next`; else pick the first task. Load the new task's `stack_limit` into PSPLIM. Return its `sp`.
5. PendSV in `global_asm!`:
   - read PSP; if EXC_RETURN bit 4 is clear, push s16–s31; push r4–r11 and lr (EXC_RETURN)
   - call `kernel_switch` with the pointer in r0
   - pop r4–r11 and lr from the returned pointer; if bit 4 clear, pop s16–s31
   - write PSP, return with `bx lr`

**Hints**
- The handler must be a global symbol named exactly `PendSV`: cortex-m-rt's vector table links to it by name. Directives: `.global PendSV`, `.type PendSV,%function`, `.thumb_func`, own `.section .text.PendSV,"ax",%progbits`.
- In `global_asm!`, register lists need doubled braces: `{{r4-r11, lr}}`.
- Conditional FP push: `tst lr, #0x10` / `it eq` / `vstmdbeq r0!, {{s16-s31}}`. Mirror order on the way out: core regs first, then FP.
- `bl kernel_switch` overwrites lr: that's fine, you already pushed it and you pop the new task's.
- The first PendSV comes from thread mode on MSP, so it "saves" junk onto the dummy PSP; `kernel_switch` ignores it because `CURRENT` is null.
- Pending: `SCB::set_pendsv()` then `cortex_m::asm::dsb(); cortex_m::asm::isb();` so the exception is taken right there.
- PSPLIM write: `core::arch::asm!("msr psplim, {}", in(reg) limit)`.
- Edition 2024 rejects `&mut STATIC_MUT`; use `&raw mut DUMMY` / raw pointers.
- `tasks` lives in `main`'s frame; that's sound because `start` never returns and handlers use MSP below it.

**Verify**
- `timeout -s INT 120 cargo run --release --bin m1_coop; echo EXIT=$?` → `a round 0`, `b round 0`, … `b round 4`, `TEST PASS`, `EXIT=0`.
- `timeout -s INT 120 cargo run --release --bin m1_overflow; echo EXIT=$?` → `EXIT=1` (probe-rs shows an escalated HardFault and a garbled unwind: expected, the frame was cut at the limit). Then `probe-rs read --chip RP235x b32 0xE000ED28 1` → `00100000` (CFSR.STKOF). `TEST PASS` here means PSPLIM is not set.

**Troubleshooting**
| Symptom | Likely cause |
|---|---|
| Hangs, no `a round 0` | `PendSV` symbol not linked (default handler loops) or never pended |
| HardFault right after start | wrong frame layout/order, or PC bit 0 set |
| `s16-s31 not preserved` | FP push/pop missing, wrong condition, or asymmetric order |
| `b ran out of turn` | ring not linked, or `CURRENT` not advanced |
| overflow exits 0 | PSPLIM not written, or written to the wrong task's limit |

---

### Task 4 (Claude): review and commit

- [ ] **Step 1:** Review `task.rs` and `arch/mod.rs` against the layout table and spec section 2 (soundness of `static mut` access, PendSV symmetry, FP condition, PSPLIM order). Report findings; the user fixes.
- [ ] **Step 2:** Run `cargo ktest`, `m1_coop`, `m1_overflow` (+ CFSR read) as in Task 3.
- [ ] **Step 3:** Commit.

```bash
git add kernel
git commit -m "feat(m1): cooperative context switch with FP and PSPLIM"
```
