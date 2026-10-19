//! Intrusive singly linked lists of tasks. The links live inside `Task`, so a
//! list never allocates or owns anything: it only threads tasks together. Which
//! field a list uses is part of its type, so a task can sit on a `next` list
//! and a `wait_next` list at the same time (a timed waiter does), and the two
//! can never be mixed up.
use core::marker::PhantomData;
use core::ptr::null_mut;

use crate::task::Task;

/// The link field a list threads through.
pub(crate) trait Link {
    /// Address of that field in `t`.
    ///
    /// # Safety
    /// `t` is a valid task.
    unsafe fn link(t: *mut Task) -> *mut *mut Task;
}

/// Ready lists and the sleep list: `Task::next`.
pub(crate) struct Next;

/// Wait lists of sync objects: `Task::wait_next`.
pub(crate) struct WaitNext;

impl Link for Next {
    unsafe fn link(t: *mut Task) -> *mut *mut Task {
        unsafe { &raw mut (*t).next }
    }
}

impl Link for WaitNext {
    unsafe fn link(t: *mut Task) -> *mut *mut Task {
        unsafe { &raw mut (*t).wait_next }
    }
}

/// Tasks blocked on one sync object, most urgent first, FIFO on ties.
pub(crate) type WaitList = TaskList<WaitNext>;

/// Tasks threaded through the link `L`, with O(1) `push_back`/`pop_front`.
///
/// Every method that takes a task requires it to be valid for as long as it
/// is on the list (tasks and sync objects are `static`).
pub(crate) struct TaskList<L: Link> {
    head: *mut Task,
    tail: *mut Task,
    _link: PhantomData<L>,
}

impl<L: Link> TaskList<L> {
    pub(crate) const fn new() -> Self {
        Self {
            head: null_mut(),
            tail: null_mut(),
            _link: PhantomData,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.head.is_null()
    }

    /// The first task, or null.
    pub(crate) fn head(&self) -> *mut Task {
        self.head
    }

    /// Appends `t`, which must be on no list of this kind.
    pub(crate) unsafe fn push_back(&mut self, t: *mut Task) {
        unsafe {
            *L::link(t) = null_mut();
            if self.tail.is_null() {
                self.head = t;
            } else {
                *L::link(self.tail) = t;
            }
            self.tail = t;
        }
    }

    /// Removes and returns the first task; null if empty.
    pub(crate) unsafe fn pop_front(&mut self) -> *mut Task {
        let t = self.head;
        if !t.is_null() {
            unsafe {
                self.head = *L::link(t);
                *L::link(t) = null_mut();
            }
            if self.head.is_null() {
                self.tail = null_mut();
            }
        }
        t
    }

    /// Inserts `t` before the first task for which `before` is true, else at
    /// the end. With `before = "less urgent than t"` equal keys keep arrival
    /// order.
    pub(crate) unsafe fn insert_sorted(&mut self, t: *mut Task, before: impl Fn(*mut Task) -> bool) {
        unsafe {
            let mut prev: *mut Task = null_mut();
            let mut at = self.head;
            while !at.is_null() && !before(at) {
                prev = at;
                at = *L::link(at);
            }
            *L::link(t) = at;
            if prev.is_null() {
                self.head = t;
            } else {
                *L::link(prev) = t;
            }
            if at.is_null() {
                self.tail = t;
            }
        }
    }

    /// Unlinks `t` if it is on this list (no-op otherwise) and clears its link.
    pub(crate) unsafe fn remove(&mut self, t: *mut Task) {
        unsafe {
            let mut prev: *mut Task = null_mut();
            let mut at = self.head;
            while !at.is_null() {
                if at == t {
                    let next = *L::link(t);
                    if prev.is_null() {
                        self.head = next;
                    } else {
                        *L::link(prev) = next;
                    }
                    if next.is_null() {
                        self.tail = prev;
                    }
                    *L::link(t) = null_mut();
                    return;
                }
                prev = at;
                at = *L::link(at);
            }
        }
    }
}
