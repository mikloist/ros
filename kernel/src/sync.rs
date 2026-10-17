use core::cell::UnsafeCell;
use core::time::Duration;

use crate::{arch::with_sched, sched::WaitList, yield_now};

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct Timeout;

pub struct Semaphore {
    inner: UnsafeCell<SemInner>,
}

struct SemInner {
    count: u32,
    waiters: WaitList,
}

// SAFETY: saying inner is only touched inside with_sched.
unsafe impl Sync for Semaphore {}

impl Semaphore {
    pub const fn new(count: u32) -> Self {
        Self {
            inner: UnsafeCell::new(SemInner {
                count: count,
                waiters: WaitList::new(),
            }),
        }
    }
    pub fn take(&self, timeout: Option<Duration>) -> Result<(), Timeout> {
        let res = with_sched(|s| {
            unsafe {
                let inner = self.inner.get();
                if (*inner).count > 0 {
                    (*inner).count -= 1; /* done */
                    Some(Ok(()))
                } else {
                    if let Some(v) = timeout
                        && v.is_zero()
                    {
                        Some(Err(Timeout))
                    } else {
                        s.block_current(&mut (*inner).waiters, timeout);
                        None
                    }
                } // same critical section
            }
        });
        if res.is_none() {
            yield_now();
            let timed_out = with_sched(|s| unsafe { (*s.current()).timed_out });
            if timed_out { Err(Timeout) } else { Ok(()) }
        } else {
            res.unwrap()
        }
    }
    pub fn give(&self) {
        let switch = with_sched(|s| unsafe {
            let inner = self.inner.get();

            let t = s.wake_one(&mut (*inner).waiters);
            if t.is_null() {
                (*inner).count += 1;
            }
            return s.should_switch();
        });

        if switch {
            yield_now();
        }
    }
}

pub struct Queue<T, const N: usize> {
    n: T,
}
impl<T, const N: usize> Queue<T, N> {
    pub const fn new() -> Self {
        todo!()
    }
    pub fn send(&self, item: T, timeout: Option<Duration>) -> Result<(), T> {
        todo!()
    }
    pub fn recv(&self, timeout: Option<Duration>) -> Result<T, Timeout> {
        todo!()
    }
    pub fn try_send(&self, item: T) -> Result<(), T> {
        todo!()
    }
    pub fn try_recv(&self) -> Result<T, Timeout> {
        todo!()
    }
}
