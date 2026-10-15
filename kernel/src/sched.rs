//! Priority scheduler: ready lists, sleep list, tick. Pure logic, host-tested.
//!
//! Invariants:
//! - The running task (`current`) is in no list.
//! - Bit `p` of `bitmap` is set iff `ready[p]` is non-empty.
//! - `sleeping` is sorted by `wake` ascending; equal `wake`s keep arrival order.
//! - A task is in at most one list at a time, linked through `Task::next`.
use core::ptr::null_mut;

use crate::task::{State, Task};

pub(crate) const PRIOS: usize = 8;

#[derive(Clone, Copy)]
struct List {
    head: *mut Task,
    tail: *mut Task,
}

impl List {
    const EMPTY: List = List {
        head: null_mut(),
        tail: null_mut(),
    };

    fn is_empty(&self) -> bool {
        self.head == null_mut()
    }

    /// Appends `t`. Caller guarantees `t` is valid and in no list.
    unsafe fn push_back(&mut self, t: *mut Task) {
        unsafe {
            (*t).next = null_mut();
            if self.tail == null_mut() {
                self.head = t;
                self.tail = t;
            } else {
                (*self.tail).next = t;
                self.tail = t;
            }
        }
    }

    /// Removes and returns the head. Caller guarantees the list is non-empty.
    unsafe fn pop_front(&mut self) -> *mut Task {
        let current = self.head;
        unsafe {
            let next = (*self.head).next;
            if next == null_mut() {
                self.tail = null_mut();
                self.head = null_mut();
            } else {
                self.head = next;
            }
        }
        return current;
    }
}

// ---- Singly linked lists threaded through a `Task` link field ----
//
// The sleep list uses `next`, wait lists use `wait_next` (a timed waiter is on
// both at once). `head` points at the list's head pointer; walking a pointer to
// the link (not to the task) makes front, middle and end the same case.

/// Which link field a list follows.
type LinkOf = unsafe fn(*mut Task) -> *mut *mut Task;

unsafe fn by_next(t: *mut Task) -> *mut *mut Task {
    unsafe { &raw mut (*t).next }
}

unsafe fn by_wait_next(t: *mut Task) -> *mut *mut Task {
    unsafe { &raw mut (*t).wait_next }
}

/// Splices `t` in before the first task where `stop` is true, or at the end.
unsafe fn insert_where(
    head: *mut *mut Task,
    t: *mut Task,
    link: LinkOf,
    stop: impl Fn(*mut Task) -> bool,
) {
    unsafe {
        let mut at = head;
        while !(*at).is_null() && !stop(*at) {
            at = link(*at);
        }
        *link(t) = *at;
        *at = t;
    }
}

/// Unlinks `t` if it is in the list (no-op otherwise) and clears its link.
unsafe fn remove(head: *mut *mut Task, t: *mut Task, link: LinkOf) {
    unsafe {
        let mut at = head;
        while !(*at).is_null() {
            if *at == t {
                *at = *link(t);
                *link(t) = null_mut();
                return;
            }
            at = link(*at);
        }
    }
}

pub(crate) struct Sched {
    ready: [List; PRIOS],
    bitmap: u8,
    sleeping: *mut Task,
    current: *mut Task,
    now: u64,
}

impl Sched {
    pub(crate) const fn new() -> Self {
        Self {
            ready: [List::EMPTY; PRIOS],
            bitmap: 0,
            sleeping: null_mut(),
            current: null_mut(),
            now: 0,
        }
    }

    /// Ticks since start.
    pub(crate) fn now(&self) -> u64 {
        return self.now;
    }

    /// The running task; null before the first `switch`.
    pub(crate) fn current(&self) -> *mut Task {
        return self.current;
    }

    /// Marks `t` Ready and appends it to the list for its `current_priority`.
    ///
    /// # Safety
    /// `t` is valid for the scheduler's lifetime and in no list.
    pub(crate) unsafe fn make_ready(&mut self, t: *mut Task) {
        unsafe {
            let prio = (*t).current_priority;
            self.ready[prio as usize].push_back(t);
            self.bitmap |= 1 << prio;
            (*t).state = State::Ready;
        }
    }

    /// Highest priority with a ready task, if any (hint: `leading_zeros`).
    fn highest(&self) -> Option<u8> {
        if self.bitmap == 0 {
            return None;
        }

        return Some(7 - self.bitmap.leading_zeros() as u8);
    }

    /// Called from PendSV. If the current task is still `Running`, puts it back
    /// at the tail of its list; then pops the head of the highest non-empty
    /// list, marks it `Running`, makes it `current` and returns it.
    pub(crate) fn switch(&mut self) -> *mut Task {
        if self.current != null_mut() {
            unsafe {
                let current = self.current;
                if (*current).state == State::Running {
                    self.make_ready(current);
                }
            }
        }

        let highest = self.highest();
        highest.expect("no ready task, idle is missing ?");
        unsafe {
            let current_task = self.ready[highest.unwrap() as usize].pop_front();
            (*current_task).state = State::Running;
            self.current = current_task;
            if self.ready[highest.unwrap() as usize].is_empty() {
                self.bitmap &= !(1 << highest.unwrap());
            }
            return current_task;
        }
    }

    /// Puts the current task to sleep until tick `until` (sorted into
    /// `sleeping`). Returns false and does nothing if `until <= now`.
    /// The caller pends PendSV when this returns true.
    pub(crate) fn sleep_current(&mut self, until: u64) -> bool {
        if until <= self.now {
            return false;
        }
        unsafe {
            let cur = self.current;
            assert!(
                !cur.is_null() && (*cur).state == State::Running,
                "sleep_current: no running task"
            );

            self.insert_sleeper(cur, until, State::Sleeping);
        }

        return true;
    }

    /// Sorted insert into `sleeping` (ties keep arrival order). `state` says
    /// why the task waits: `Sleeping`, or `Blocked` for a timed wait.
    unsafe fn insert_sleeper(&mut self, t: *mut Task, until: u64, state: State) {
        unsafe {
            (*t).state = state;
            (*t).wake = until;
            insert_where(&mut self.sleeping, t, by_next, |o| (*o).wake > until);
        }
    }

    /// Unlinks `t` from `sleeping` if it is there (cancels a timeout).
    unsafe fn remove_sleeper(&mut self, t: *mut Task) {
        unsafe { remove(&mut self.sleeping, t, by_next) };
    }

    /// Called from SysTick. Advances `now`, makes every sleeper with
    /// `wake <= now` ready, and returns true if a ready task's priority is
    /// >= the current task's (preempt, or time slice over). False before the
    /// first switch.
    pub(crate) fn tick(&mut self) -> bool {
        self.now += 1;
        unsafe {
            while !self.sleeping.is_null() && (*self.sleeping).wake <= self.now {
                let c = self.sleeping;
                self.sleeping = (*c).next;
                if (*c).state == State::Blocked {
                    // Timed wait expired before the event: leave the wait list.
                    remove(&mut (*(*c).waiting_on).head, c, by_wait_next);
                    (*c).waiting_on = null_mut();
                    (*c).timed_out = true;
                }
                self.make_ready(c);
            }
            return self.should_switch();
        }
    }

    /// Blocks the current task on `wl`: state Blocked, `timed_out = false`,
    /// `waiting_on = wl`, inserted after every waiter with priority >= its own
    /// (via `wait_next`). With `Some(ticks)` it also goes on the sleep list at
    /// `now + ticks`. The caller pends PendSV.
    ///
    /// # Safety
    /// `wl` outlives the wait (sync objects are `static`).
    pub(crate) unsafe fn block_current(&mut self, wl: *mut WaitList, timeout: Option<u64>) {
        assert!(self.current != null_mut(), "block_current: no running task");
        assert!(
            unsafe { (*self.current).state == State::Running },
            "block_current: no running task"
        );

        unsafe {
            let c = self.current;

            (*c).state = State::Blocked;
            (*c).waiting_on = wl;
            (*c).timed_out = false;

            // Most urgent first; stop before the first *less* urgent waiter,
            // so equal priorities stay in arrival order.
            let p = (*c).current_priority;
            insert_where(&mut (*wl).head, c, by_wait_next, |o| (*o).current_priority < p);

            if let Some(v) = timeout {
                self.insert_sleeper(c, self.now + v, State::Blocked);
            }
        }
    }

    /// Pops the most urgent waiter off `wl`, cancels its timeout (removes it
    /// from the sleep list), makes it ready and returns it; null if none.
    ///
    /// # Safety
    /// `wl` is a live wait list.
    pub(crate) unsafe fn wake_one(&mut self, wl: *mut WaitList) -> *mut Task {
        if wl.is_null() {
            return null_mut();
        }

        unsafe {
            let t = (*wl).head;
            if t.is_null() {
                return null_mut();
            }
            (*wl).head = (*t).wait_next;
            (*t).wait_next = null_mut();
            (*t).waiting_on = null_mut();
            self.remove_sleeper(t);
            self.make_ready(t);
            return t;
        }
    }

    /// True if a ready task's priority is >= the current task's (false before
    /// the first switch). `tick` should end with this.
    pub(crate) fn should_switch(&self) -> bool {
        unsafe {
            if !self.current.is_null() {
                return Some((*self.current).current_priority) <= self.highest();
            } else {
                return false;
            }
        }
    }
}

/// Tasks blocked on one sync object, linked through `Task::wait_next`, most
/// urgent first; equal priorities in arrival order.
pub(crate) struct WaitList {
    head: *mut Task,
}

impl WaitList {
    pub(crate) const fn new() -> Self {
        WaitList { head: null_mut() }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.head.is_null()
    }
}

#[cfg(test)]
mod tests;
