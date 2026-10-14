# ros: RTOS kernel for Raspberry Pi Pico 2 W — design

Learning project: write a preemptive RTOS kernel in Rust for the RP2350 (Pico 2 W).

## Decisions

| Topic | Choice |
|---|---|
| Depth | Kernel from scratch; borrow platform (`cortex-m-rt`, `rp235x-hal`). Embassy removed. |
| Cores | Arm Cortex-M33 (`thumbv8m.main-none-eabihf`), Secure state, no TrustZone setup. |
| SMP | Core 0 only. Critical sections behind one `arch` API so SMP can swap them later. |
| v1 scope | Levels 1–4: context switch, preemptive priority scheduler, semaphore/mutex/queue, SVC + unprivileged tasks + MPU. |
| Structure | FreeRTOS-style, static task table, shared address space, no heap. Scheduler handles `*mut Task` only, so runtime task creation (heap-backed) can be added later without rewriting it. |
| Debug I/O | `defmt` over RTT, `probe-rs run`. External LED on GP15 (onboard LED needs CYW43). |

Out of scope for v1 (each gets its own spec): runtime task creation, CYW43 Wi-Fi, second core, tickless idle.

## 1. Layout and boot

```
ros/
├─ Cargo.toml          workspace: kernel, app
├─ kernel/             lib, no_std; host-testable except arch/
│  └─ src/
│     ├─ lib.rs        public API: start(), sleep(), sleep_until(), yield_now(), now(), sync types
│     ├─ arch/
│     │  ├─ mod.rs     PendSV, SysTick, SVC entry, critical section, fault handlers
│     │  └─ mpu.rs
│     ├─ task.rs
│     ├─ sched.rs
│     ├─ time.rs
│     ├─ sync.rs
│     └─ syscall.rs
└─ app/                bin; board init, task table, demos
   ├─ src/main.rs
   ├─ src/bin/         one demo per milestone
   ├─ memory.x, build.rs, .cargo/config.toml   (moved from root)
   └─ cyw43-firmware/  (kept for later Wi-Fi spec)
```

- Boot: `cortex-m-rt` reset/vector table; `rp235x-hal` provides `IMAGE_DEF` block, clocks (150 MHz), GPIO, UART.
- Only `arch/` touches registers. Its interface to the rest of the kernel: start first task, pend a switch, start tick, enter/exit critical section.
- `sched`, `time`, `sync` are pure logic and run under `cargo test` on the host.

## 2. Tasks and context switch

```rust
pub struct Task {
    sp: *mut u32,          // saved PSP
    stack_limit: *mut u32, // PSPLIM
    prio: u8,              // effective (may be inherited)
    base_prio: u8,
    privileged: bool,      // level 4: applied to CONTROL.nPRIV on switch
    state: State,          // Ready | Running | Sleeping(wake_tick) | Blocked | Dead
    next: *mut Task,       // ready/wait list link
    name: &'static str,
}
```

- App declares tasks as statics: `Stack<N>` (`#[repr(align(32))]`) + `fn() -> !` entry. `kernel::start(&mut [&mut Task]) -> !`.
- 8 priorities, higher = more urgent. Priority 0 reserved for the idle task; apps use 1–7. Ready set = `u8` bitmap + per-priority list; highest via CLZ.
- PendSV (asm, lowest exception priority):
  1. Hardware has stacked R0–R3, R12, LR, PC, xPSR (+ S0–S15, FPSCR if FP active; lazy stacking).
  2. Push R4–R11 + EXC_RETURN to PSP; if EXC_RETURN bit 4 == 0 also push S16–S31.
  3. Store `sp` to current task; call `sched::switch()`.
  4. Load next `sp`, `stack_limit` into PSP/PSPLIM, set CONTROL.nPRIV, (level 4) load MPU regions, pop, `bx` EXC_RETURN.
- Initial frame: xPSR `0x0100_0000`, PC = entry, LR = trap that panics, EXC_RETURN `0xFFFF_FFFD`.
- Start: set PSP to a dummy area, mark no current task, pend PendSV. No special first-task path.
- Stack overflow: PSPLIM → UsageFault (STKOF), enabled via SHCSR. See section 5 for fault policy.

## 3. Scheduling and time

- Highest-priority ready task runs. A wakeup that outranks the running task pends PendSV immediately.
- Same-priority tasks round-robin with a 1-tick time slice.
- Idle task (priority 0): `loop { wfi() }`.
- SysTick at 1 kHz from the core clock. `u64` tick count, read inside a critical section; never wraps.
- Sleepers sit in a list sorted by wake tick; SysTick only examines the head, wakes due tasks, rotates round-robin, pends PendSV if needed.
- API: `now() -> u64`, `sleep(ms)`, `sleep_until(tick)`, `yield_now()`.
- Critical section: save PRIMASK + `cpsid i`, restore on exit. The `critical-section` crate implementation comes from `cortex-m`'s `critical-section-single-core` feature (needed by `defmt-rtt`).
- Host tests: bitmap highest pick, round-robin order, sorted sleep insert, multiple wakeups on one tick.

## 4. Synchronization

Common:
- Each object has a wait list ordered by priority, FIFO within a priority.
- Blocking calls take `timeout: Option<u64>` (ticks; `None` = forever). Timed waiters are on both the wait list and the sleep list; whichever fires first removes the task from both. Timeout returns `Err(Timeout)`.
- Handoff on wake: the woken task receives the unit/lock/item directly, so no other task can grab it first.

Semaphore (counting): `new(count)`, `take(timeout) -> Result<(), Timeout>`, `give()`. `give()` may be called from interrupt handlers.

`Mutex<T>`: `lock(timeout) -> Result<Guard<T>, Timeout>`; guard unlocks on drop.
- Tracks owner. Locking again from the owner panics (not recursive).
- Priority inheritance: a higher-priority waiter raises the owner's effective priority; propagates through the owner's `blocked_on` mutex chain.
- Unlock sets the owner's priority to max(`base_prio`, highest waiter on any other mutex it still holds).
- Task context only.

`Queue<T, N>`: fixed ring buffer. `send(item, timeout)`, `recv(timeout)` block on full/empty. `try_send`, `try_recv` never block and may be called from interrupt handlers.

Host tests: wait-list order, timeout vs. event race, ring wraparound, inheritance chain, restore with two held mutexes.

## 5. Protection (level 4)

Syscalls:
- Public API signatures unchanged. Levels 1–3 call the kernel directly. Level 4 wrappers execute `svc #N`.
- SVC handler: number from the immediate of the instruction before stacked PC; args from stacked R0–R3; result written to stacked R0.
- Blocking syscalls only pend PendSV; the result is written to the task's stacked R0 at wakeup.

MPU (ARMv8-M PMSA, 8 regions, base/limit at 32-byte granularity, PRIVDEFENA = 1):
- Per task: flash RX; own stack RW + XN; the `.shared` RW region (holds all task-shared data, including every `Mutex<T>`'s `T`); peripheral ranges listed in the task declaration (at most 5 regions remain for these).
- Regions reloaded in PendSV on each switch.
- Kernel objects (sync objects, wait lists, TCBs) live in `.kobj`, privileged-only. Syscalls validate pointers: object pointers must lie inside `.kobj`; buffer pointers must lie inside the caller's regions.

Fault policy (M2–M4: every fault logs and halts; task kill from M5, which needs it):
- Fault in task context (MemManage, BusFault, UsageFault incl. STKOF): log task name + fault, set `Dead`, remove from all lists, switch away.
- Fault in handler/kernel context: log and halt.
- Interrupt handlers stay privileged and talk to tasks via `give()` / `try_send`.

## 6. Testing and milestones

- Host: `cargo test -p kernel` for pure logic.
- Board: one binary per milestone in `app/src/bin/`, run via `probe-rs run`. Tasks verify with `assert!`; failure panics through `panic-probe`. Success ends with `bkpt`; whether `probe-rs run` exits 0 on `bkpt` is unverified and is checked in M0 (fallback: semihosting exit).

| # | Goal | Proof |
|---|---|---|
| M0 | Workspace, `rp235x-hal` boot, `defmt` | GP15 LED blinks via busy-wait; pass/fail exit mechanism confirmed |
| M1 | Cooperative context switch | Two tasks alternate logs via `yield_now` |
| M2 | SysTick, priorities, sleep, idle | Sleep timing within 1 tick; higher-priority wakeup preempts immediately |
| M3 | Semaphore, queue, interrupt → task | GPIO button IRQ `give()`s to a task; producer/consumer over a queue |
| M4 | Mutex + priority inheritance | Classic low/medium/high inversion scenario resolved; asserts check ordering |
| M5 | SVC + unprivileged tasks | Whole API via syscalls; unprivileged SysTick write faults and kills only that task |
| M6 | MPU | Bad pointer kills only the offending task; others keep running |
