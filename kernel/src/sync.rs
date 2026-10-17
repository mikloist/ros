use core::cell::UnsafeCell;
use core::time::Duration;

use crate::{arch::with_sched, ring::Ring, sched::WaitList, yield_now};

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
    inner: UnsafeCell<QueueInner<T, N>>,
}

struct QueueInner<T, const N: usize> {
    ring: Ring<T, N>,
    receivers: WaitList,
    senders: WaitList,
}
// SAFETY: `inner` is only accessed inside `with_sched` (interrupts masked), so
// accesses never overlap. Items move between tasks, hence `T: Send`.
unsafe impl<T: Send, const N: usize> Sync for Queue<T, N> {}

impl<T, const N: usize> Queue<T, N> {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(QueueInner {
                ring: Ring::new(),
                receivers: WaitList::new(),
                senders: WaitList::new(),
            }),
        }
    }

    pub fn send(&self, item: T, timeout: Option<Duration>) -> Result<(), T> {
        let mut slot = Some(item);
        let slot_ptr = &raw mut slot;
        let (done, switch) = with_sched(|s| unsafe {
            let inner = self.inner.get();
            let item = (*slot_ptr).take().unwrap();
            let r = s.wake_one(&mut (*inner).receivers);
            if !r.is_null() {
                *((*r).msg as *mut Option<T>) = Some(item);
                return (Some(Ok(())), s.should_switch());
            }
            match (*inner).ring.push(item) {
                Ok(()) => (Some(Ok(())), false),
                Err(item) if timeout == Some(Duration::ZERO) => (Some(Err(item)), false),
                Err(item) => {
                    *slot_ptr = Some(item);
                    (*s.current()).msg = slot_ptr as *mut ();
                    s.block_current(&mut (*inner).senders, timeout);
                    (None, false)
                }
            }
        });

        if let Some(result) = done {
            if switch {
                yield_now();
            }
            return result;
        }
        yield_now(); // blocked: returns once a receiver took the item, or on timeout
        let timed_out = with_sched(|s| unsafe { (*s.current()).timed_out });
        if timed_out {
            // Nobody took it: the item is still parked in our slot.
            Err(slot.take().expect("timed-out sender lost its item"))
        } else {
            Ok(())
        }
    }

    pub fn recv(&self, timeout: Option<Duration>) -> Result<T, Timeout> {
        let mut slot: Option<T> = None;
        let slot_ptr = &raw mut slot;
        let (done, switch) = with_sched(|s| unsafe {
            let inner = self.inner.get();
            match (*inner).ring.pop() {
                Some(v) => {
                    let w = s.wake_one(&mut (*inner).senders);
                    if !w.is_null() {
                        let parked = (*((*w).msg as *mut Option<T>)).take().unwrap();
                        let _ = (*inner).ring.push(parked); // cannot fail: we just popped
                        (Some(Ok(v)), s.should_switch())
                    } else {
                        (Some(Ok(v)), false)
                    }
                }
                None => {
                    if timeout == Some(Duration::ZERO) {
                        (Some(Err(Timeout {})), false)
                    } else {
                        (*s.current()).msg = slot_ptr as *mut ();
                        s.block_current(&mut (*inner).receivers, timeout);
                        (None, false)
                    }
                }
            }
        });

        if let Some(result) = done {
            if switch {
                yield_now();
            }
            return result;
        }
        yield_now(); // blocked: returns once a sender delivered, or on timeout
        let timed_out = with_sched(|s| unsafe { (*s.current()).timed_out });
        if timed_out {
            Err(Timeout {})
        } else {
            Ok(slot.take().expect("woken receiver got no item"))
        }
    }

    pub fn try_send(&self, item: T) -> Result<(), T> {
        self.send(item, Some(Duration::ZERO))
    }
    pub fn try_recv(&self) -> Result<T, Timeout> {
        self.recv(Some(Duration::ZERO))
    }
}
