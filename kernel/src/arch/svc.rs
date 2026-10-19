use defmt::{error, info};

use crate::sched::{Instant, MutexCore, ticks_to_timeout};
use crate::sync::{QueueOps, SemInner, mutex_lock, mutex_unlock, sem_give, sem_take};
use crate::task::State;
use core::arch::global_asm;
use core::time::Duration;

use crate::arch::with_sched;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sys {
    Yield,
    Now,
    SleepUntil,
    Sleep,
    SemTake,
    SemGive,
    MutexLock,
    MutexUnlock,
    QueueSend,
    QueueRecv,
    CsEnter,
    CsExit,
}
/// Checked decode of the byte read from the `svc` instruction (untrusted:
/// any task can execute `svc #200`). `Err` carries the bad number.
impl TryFrom<u8> for Sys {
    type Error = u8;
    fn try_from(n: u8) -> Result<Sys, u8> {
        use Sys::*;
        // Indexed by discriminant: keep in the same order as the enum.
        [
            Yield,
            Now,
            SleepUntil,
            Sleep,
            SemTake,
            SemGive,
            MutexLock,
            MutexUnlock,
            QueueSend,
            QueueRecv,
            CsEnter,
            CsExit,
        ]
        .get(n as usize)
        .copied()
        .ok_or(n)
    }
}

/// Syscall result, written to the caller's stacked r0.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Ok,
    Timeout,
    /// The caller blocked; replaced by Ok/Timeout at switch-in, never seen by a task.
    Blocked,
}
impl From<Status> for u32 {
    fn from(s: Status) -> u32 {
        s as u32
    }
}

#[inline(always)]
pub(crate) fn svc<const N: u8>(a0: u32, a1: u32, a2: u32, a3: u32) -> (u32, u32) {
    let (r0, r1);
    unsafe {
        core::arch::asm!(
            "svc {n}",
            n = const N,                       // N pasted into the instruction
            inlateout("r0") a0 => r0,          // r0 in: arg, out: result
            inlateout("r1") a1 => r1,
            in("r2") a2,
            in("r3") a3,
            options(nostack),
        );
    }
    (r0, r1)
}

pub(crate) fn in_handler() -> bool {
    let ipsr: u32;
    unsafe {
        core::arch::asm!("mrs {}, ipsr", out(reg) ipsr, options(nomem, nostack, preserves_flags))
    };
    ipsr != 0
}

use critical_section::RawRestoreState;

/// Critical sections for everyone, `defmt` and the HAL included.
/// - Unprivileged task: `cpsid` is ignored there, so ask the kernel to raise
///   BASEPRI (masks SysTick and PendSV). State = 0x100 | old BASEPRI.
/// - Privileged code (handlers, `main`, idle, test tasks): mask interrupts
///   with PRIMASK. State = 1 if they were enabled before, else 0.
struct MyCriticalSection;
critical_section::set_impl!(MyCriticalSection);

/// Bit 8 of the restore state: this section was entered through `CsEnter`.
const VIA_SVC: u32 = 0x100;

fn unprivileged_task() -> bool {
    use cortex_m::register::control::{self, Npriv};
    !in_handler() && control::read().npriv() == Npriv::Unprivileged
}

unsafe impl critical_section::Impl for MyCriticalSection {
    unsafe fn acquire() -> RawRestoreState {
        if unprivileged_task() {
            let (old, _) = svc::<{ Sys::CsEnter as u8 }>(0, 0, 0, 0);
            return VIA_SVC | old;
        }
        // cortex-m's `Active` means exceptions are active, i.e. not masked.
        let was_enabled = cortex_m::register::primask::read().is_active();
        cortex_m::interrupt::disable();
        was_enabled as u32
    }

    unsafe fn release(state: RawRestoreState) {
        if state & VIA_SVC != 0 {
            svc::<{ Sys::CsExit as u8 }>(state & 0xFF, 0, 0, 0);
        } else if state == 1 {
            // Only re-enable if they were on before: nested sections stay masked.
            unsafe { cortex_m::interrupt::enable() };
        }
    }
}

global_asm!(
    ".section .text.SVCall,\"ax\",%progbits",
    ".global SVCall", // cortex-m-rt's vector table links to this name
    ".type SVCall,%function",
    ".thumb_func",
    "SVCall:",
    "  tst lr, #4",     // EXC_RETURN bit 2: which stack did the caller use?
    "  ite eq",         // if-then-else for the next two instructions
    "  mrseq r0, msp",  // bit 2 == 0: caller was on MSP (main, before start)
    "  mrsne r0, psp",  // bit 2 == 1: caller was a task, on PSP
    "  b svc_dispatch", // tail-jump; r0 = frame pointer = first argument
);

fn join(lo: u64, hi: u64) -> u64 {
    return hi << 32 | lo;
}

/// Splits a u64 into (lo, hi) syscall words; `join` puts them back.
pub(crate) fn split(v: u64) -> (u32, u32) {
    (v as u32, (v >> 32) as u32)
}

/// Runs a kernel half from an interrupt handler (already privileged, so no
/// `svc`). Handlers cannot wait; pend a switch if someone more urgent woke.
pub(crate) fn from_handler(f: impl FnOnce(&mut crate::sched::Sched) -> Status) -> Status {
    with_sched(|s| {
        let r = f(s);
        assert_ne!(r, Status::Blocked, "an interrupt handler must not block");
        if s.should_switch() {
            cortex_m::peripheral::SCB::set_pendsv();
        }
        r
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn svc_dispatch(frame: *mut u32) {
    unsafe {
        let n = *((*frame.add(6)) as *const u8).sub(2); // after calling svc the CPU does not tell you
        // what # was called, this is getting the svc
        // argument from the stack

        with_sched(|s| {
            let v = Sys::try_from(n);

            if v.is_err() {
                error!("Invalid svc call #{}", n);
                s.kill_current();
                cortex_m::peripheral::SCB::set_pendsv();
                return;
            }

            match v.unwrap() {
                Sys::Yield => {
                    cortex_m::peripheral::SCB::set_pendsv();
                    *frame.add(0) = Status::Ok as u32
                }

                Sys::Now => {
                    let n = s.now().ticks();
                    *frame.add(1) = (n >> 32) as u32;
                    *frame.add(0) = (n) as u32;
                }
                Sys::SleepUntil => {
                    let lo = *frame.add(0) as u64; // r0: low 32 bits
                    let hi = *frame.add(1) as u64; // r1: high 32 bits
                    s.sleep_current(Instant::from_ticks(join(lo, hi)));
                    *frame.add(0) = Status::Ok as u32;
                }
                Sys::Sleep => {
                    let now = s.now();
                    let lo = *frame.add(0) as u64; // r0: low 32 bits
                    let hi = *frame.add(1) as u64; // r1: high 32 bits
                    s.sleep_current(now + Duration::from_millis(join(lo, hi)));
                    *frame.add(0) = Status::Ok as u32;
                }
                Sys::SemTake => {
                    let inner = *frame.add(0) as *mut SemInner;
                    let timeout =
                        ticks_to_timeout(join(*frame.add(1) as u64, *frame.add(2) as u64));
                    *frame.add(0) = sem_take(s, inner, timeout) as u32;
                }
                Sys::SemGive => {
                    *frame.add(0) = sem_give(s, *frame.add(0) as *mut SemInner) as u32;
                }
                Sys::MutexLock => {
                    let m = *frame.add(0) as *mut MutexCore;
                    let timeout =
                        ticks_to_timeout(join(*frame.add(1) as u64, *frame.add(2) as u64));
                    *frame.add(0) = mutex_lock(s, m, timeout) as u32;
                }
                Sys::MutexUnlock => {
                    *frame.add(0) = mutex_unlock(s, *frame.add(0) as *mut MutexCore) as u32;
                }
                Sys::QueueSend | Sys::QueueRecv => {
                    // A queue starts with its `&'static QueueOps` (repr(C)).
                    let q = *frame.add(0) as *const ();
                    let ops = *(q as *const &'static QueueOps);
                    let op = if v == Ok(Sys::QueueSend) {
                        ops.send
                    } else {
                        ops.recv
                    };
                    let slot = *frame.add(1) as *mut ();
                    let timeout =
                        ticks_to_timeout(join(*frame.add(2) as u64, *frame.add(3) as u64));
                    *frame.add(0) = op(s, q, slot, timeout) as u32;
                }
                Sys::CsEnter => {
                    let current: u8 = cortex_m::register::basepri::read();
                    cortex_m::register::basepri::write(0x80); // blocks everything with priority
                    // 0x80–0xFF; 0x00–0x7F still run,
                    // ensures we are in critical section
                    *frame.add(0) = current as u32;
                }
                Sys::CsExit => {
                    let old_pri = *frame.add(0) as u32; // r0: low 32 bits
                    cortex_m::register::basepri::write(old_pri as u8);
                    *frame.add(0) = Status::Ok as u32;
                }
            }

            let t = s.current();

            if t.is_null() {
                return;
            }

            let state = (*t).state;

            if state == State::Blocked {
                (*t).syscall_frame = frame;
            }
            if state != State::Running || s.should_switch() {
                cortex_m::peripheral::SCB::set_pendsv();
            }
        });
    }
}
