//! Counting semaphore; `give` is safe from interrupt handlers.
use core::cell::UnsafeCell;
use core::time::Duration;

use super::{Timeout, result};
use crate::arch::svc::{Status, Sys, from_handler, in_handler, split, svc};
use crate::sched::{Sched, WaitList, timeout_to_ticks};

pub struct Semaphore {
    inner: UnsafeCell<SemInner>,
}

pub(crate) struct SemInner {
    count: u32,
    waiters: WaitList,
}

// SAFETY: `inner` is only touched by the kernel halves, with the scheduler locked.
unsafe impl Sync for Semaphore {}

/// Kernel half of `take`: a unit if there is one, else fail (zero timeout) or block.
pub(crate) unsafe fn sem_take(s: &mut Sched, inner: *mut SemInner, timeout: Option<Duration>) -> Status {
    unsafe {
        if (*inner).count > 0 {
            (*inner).count -= 1;
            Status::Ok
        } else if timeout == Some(Duration::ZERO) {
            Status::Timeout
        } else {
            s.block_current(&mut (*inner).waiters, timeout);
            Status::Blocked
        }
    }
}

/// Kernel half of `give`: hand the unit to the most urgent waiter, else count up.
pub(crate) unsafe fn sem_give(s: &mut Sched, inner: *mut SemInner) -> Status {
    unsafe {
        if s.wake_one(&mut (*inner).waiters).is_null() {
            (*inner).count += 1;
        }
    }
    Status::Ok
}

impl Semaphore {
    pub const fn new(count: u32) -> Self {
        Self {
            inner: UnsafeCell::new(SemInner {
                count,
                waiters: WaitList::new(),
            }),
        }
    }

    /// Task context only.
    pub fn take(&self, timeout: Option<Duration>) -> Result<(), Timeout> {
        let (lo, hi) = split(timeout_to_ticks(timeout));
        result(svc::<{ Sys::SemTake as u8 }>(self.inner.get() as u32, lo, hi, 0).0)
    }

    /// Safe from interrupt handlers.
    pub fn give(&self) {
        if in_handler() {
            from_handler(|s| unsafe { sem_give(s, self.inner.get()) });
        } else {
            svc::<{ Sys::SemGive as u8 }>(self.inner.get() as u32, 0, 0, 0);
        }
    }
}
