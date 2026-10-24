//! Fixed-capacity queue with direct handoff; `try_send`/`try_recv` are safe
//! from interrupt handlers.
use core::cell::UnsafeCell;
use core::time::Duration;

use super::{Timeout, result};
use crate::arch::svc::{Status, Sys, from_handler, in_handler, split, svc};
use crate::collections::ring::Ring;
use crate::sched::{Sched, WaitList, timeout_to_ticks};

/// A queue's kernel halves, compiled for its own `T` and `N`. The dispatcher
/// does not know `T`, so it calls them through the pointer at the start of the
/// queue. Arguments: the queue, the caller's `Option<T>` slot, the timeout.
pub(crate) struct QueueOps {
    pub(crate) send: unsafe fn(&mut Sched, *const (), *mut (), Option<Duration>) -> Status,
    pub(crate) recv: unsafe fn(&mut Sched, *const (), *mut (), Option<Duration>) -> Status,
    /// Size of the caller's slot (`Option<T>`), for the dispatcher's pointer check.
    pub(crate) slot_size: usize,
}

/// `ops` first (`repr(C)`), so the dispatcher finds it without knowing `T`.
#[repr(C)]
pub struct Queue<T, const N: usize> {
    ops: &'static QueueOps,
    inner: UnsafeCell<QueueInner<T, N>>,
}

struct QueueInner<T, const N: usize> {
    ring: Ring<T, N>,
    receivers: WaitList,
    senders: WaitList,
}

// SAFETY: `inner` is only touched by the kernel halves, with the scheduler
// locked. Items move between tasks, hence `T: Send`.
unsafe impl<T: Send, const N: usize> Sync for Queue<T, N> {}

impl<T, const N: usize> Queue<T, N> {
    const OPS: QueueOps = QueueOps {
        send: Self::send_raw,
        recv: Self::recv_raw,
        slot_size: core::mem::size_of::<Option<T>>(),
    };

    pub const fn new() -> Self {
        Self {
            // A reference to a constant is promoted to `'static`.
            ops: &Self::OPS,
            inner: UnsafeCell::new(QueueInner {
                ring: Ring::new(),
                receivers: WaitList::new(),
                senders: WaitList::new(),
            }),
        }
    }

    /// Kernel half of `send`. The item goes to a waiting receiver, else into
    /// the ring; if full it stays in the caller's slot (parked there if the
    /// caller blocks, so a receiver can collect it).
    unsafe fn send_raw(s: &mut Sched, q: *const (), slot: *mut (), timeout: Option<Duration>) -> Status {
        unsafe {
            let inner = (*(q as *const Self)).inner.get();
            let slot = slot as *mut Option<T>;
            let r = s.wake_one(&mut (*inner).receivers);
            if !r.is_null() {
                *((*r).msg as *mut Option<T>) = (*slot).take();
                return Status::Ok;
            }
            match (*inner).ring.push((*slot).take().unwrap()) {
                Ok(()) => Status::Ok,
                Err(item) => {
                    *slot = Some(item);
                    if timeout == Some(Duration::ZERO) {
                        Status::Timeout
                    } else {
                        (*s.current()).msg = slot as *mut ();
                        s.block_current(&mut (*inner).senders, timeout);
                        Status::Blocked
                    }
                }
            }
        }
    }

    /// Kernel half of `recv`: the item lands in the caller's slot, now or
    /// (if it blocks) when a sender delivers it.
    unsafe fn recv_raw(s: &mut Sched, q: *const (), slot: *mut (), timeout: Option<Duration>) -> Status {
        unsafe {
            let inner = (*(q as *const Self)).inner.get();
            let slot = slot as *mut Option<T>;
            match (*inner).ring.pop() {
                Some(v) => {
                    *slot = Some(v);
                    let w = s.wake_one(&mut (*inner).senders);
                    if !w.is_null() {
                        let parked = (*((*w).msg as *mut Option<T>)).take().unwrap();
                        let _ = (*inner).ring.push(parked); // cannot fail: we just popped
                    }
                    Status::Ok
                }
                None if timeout == Some(Duration::ZERO) => Status::Timeout,
                None => {
                    (*s.current()).msg = slot as *mut ();
                    s.block_current(&mut (*inner).receivers, timeout);
                    Status::Blocked
                }
            }
        }
    }

    /// Runs one op: directly from an interrupt handler (non-blocking only),
    /// else as a syscall. Returns r0.
    fn call(&self, send: bool, slot: *mut (), timeout: Option<Duration>) -> u32 {
        let q = self as *const Self as *const ();
        if in_handler() {
            assert!(timeout == Some(Duration::ZERO), "interrupt handlers may only try_send/try_recv");
            let op = if send { self.ops.send } else { self.ops.recv };
            return from_handler(|s| unsafe { op(s, q, slot, timeout) }) as u32;
        }
        let (lo, hi) = split(timeout_to_ticks(timeout));
        if send {
            svc::<{ Sys::QueueSend as u8 }>(q as u32, slot as u32, lo, hi).0
        } else {
            svc::<{ Sys::QueueRecv as u8 }>(q as u32, slot as u32, lo, hi).0
        }
    }

    /// Blocks while full (task context); `Err` gives the item back.
    pub fn send(&self, item: T, timeout: Option<Duration>) -> Result<(), T> {
        let mut slot = Some(item);
        match result(self.call(true, &raw mut slot as *mut (), timeout)) {
            Ok(()) => Ok(()),
            // Not delivered: the item is still in our slot.
            Err(Timeout) => Err(slot.take().expect("unsent item missing from its slot")),
        }
    }

    /// Blocks while empty (task context).
    pub fn recv(&self, timeout: Option<Duration>) -> Result<T, Timeout> {
        let mut slot: Option<T> = None;
        result(self.call(false, &raw mut slot as *mut (), timeout))?;
        Ok(slot.take().expect("received item missing from its slot"))
    }

    /// Never blocks; safe from interrupt handlers.
    pub fn try_send(&self, item: T) -> Result<(), T> {
        self.send(item, Some(Duration::ZERO))
    }

    /// Never blocks; safe from interrupt handlers.
    pub fn try_recv(&self) -> Result<T, Timeout> {
        self.recv(Some(Duration::ZERO))
    }
}
