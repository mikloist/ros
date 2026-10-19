//! Semaphore, Queue, Mutex. Each operation has a kernel half (runs inside
//! `svc_dispatch` or `from_handler`, scheduler already locked, returns a
//! `Status`) and a task half (encodes the arguments and makes the syscall).
use core::time::Duration;
use core::{cell::UnsafeCell, marker::PhantomData};

use crate::arch::svc::{Status, Sys, from_handler, in_handler, split, svc};
use crate::ring::Ring;
use crate::sched::{Lock, MutexCore, Sched, WaitList, timeout_to_ticks};

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct Timeout;

/// Task side: turns the syscall's r0 back into a `Result`.
fn result(r0: u32) -> Result<(), Timeout> {
    if r0 == Status::Ok as u32 { Ok(()) } else { Err(Timeout) }
}

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

/// A queue's kernel halves, compiled for its own `T` and `N`. The dispatcher
/// does not know `T`, so it calls them through the pointer at the start of the
/// queue. Arguments: the queue, the caller's `Option<T>` slot, the timeout.
pub(crate) struct QueueOps {
    pub(crate) send: unsafe fn(&mut Sched, *const (), *mut (), Option<Duration>) -> Status,
    pub(crate) recv: unsafe fn(&mut Sched, *const (), *mut (), Option<Duration>) -> Status,
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

/// Kernel half of `lock`.
pub(crate) unsafe fn mutex_lock(s: &mut Sched, m: *mut MutexCore, timeout: Option<Duration>) -> Status {
    match unsafe { s.lock_current(m, timeout) } {
        Lock::Acquired => Status::Ok,
        Lock::WouldBlock => Status::Timeout,
        Lock::Blocked => Status::Blocked,
    }
}

/// Kernel half of unlock (guard drop). Whether to switch is the dispatcher's call.
pub(crate) unsafe fn mutex_unlock(s: &mut Sched, m: *mut MutexCore) -> Status {
    unsafe { s.unlock_current(m) };
    Status::Ok
}

pub struct Mutex<T> {
    core: UnsafeCell<MutexCore>,
    data: UnsafeCell<T>,
}

/// Unlocks on drop. `!Send`: only the owner may unlock.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    _not_send: PhantomData<*const ()>,
}

impl<'a, T> MutexGuard<'a, T> {
    const fn new(mutex: &'a Mutex<T>) -> Self {
        Self {
            mutex,
            _not_send: PhantomData,
        }
    }
}

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard exists only while this task owns the mutex.
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as above; `&mut self` rules out other borrows through this guard.
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        svc::<{ Sys::MutexUnlock as u8 }>(self.mutex.core.get() as u32, 0, 0, 0);
    }
}

// SAFETY: `core` is only touched by the kernel halves; `data` only through a
// guard, which exists for one task at a time.
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            core: UnsafeCell::new(MutexCore::new()),
            data: UnsafeCell::new(data),
        }
    }

    /// Task context only.
    pub fn lock(&self, timeout: Option<Duration>) -> Result<MutexGuard<'_, T>, Timeout> {
        assert!(!in_handler(), "Mutex::lock from an interrupt");
        let (lo, hi) = split(timeout_to_ticks(timeout));
        result(svc::<{ Sys::MutexLock as u8 }>(self.core.get() as u32, lo, hi, 0).0)?;
        Ok(MutexGuard::new(self))
    }
}
