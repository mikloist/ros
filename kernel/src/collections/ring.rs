//! Fixed-capacity FIFO ring buffer: the storage behind `Queue`. Pure, host-tested.
//!
//! Items live in slots `head, head+1, .., head+len-1` (mod N); those slots are
//! initialized, every other slot is not.
use core::mem::MaybeUninit;

pub(crate) struct Ring<T, const N: usize> {
    buf: [MaybeUninit<T>; N],
    head: usize, // index of the oldest item
    len: usize,  // number of items stored
}

impl<T, const N: usize> Ring<T, N> {
    /// Empty ring. `const` so a `static Queue` can hold one.
    /// Assert `N > 0` (N == 0 would divide by zero in `% N`).
    pub(crate) const fn new() -> Self {
        assert!(N > 0);
        Self {
            buf: [const { MaybeUninit::uninit() }; N],
            head: 0,
            len: 0,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn is_full(&self) -> bool {
        self.len == N
    }

    fn get_pos(&self) -> usize {
        return self.head + self.len;
    }

    /// Appends `item` at slot `(head + len) % N`, or hands it back if full.
    pub(crate) fn push(&mut self, item: T) -> Result<(), T> {
        if !self.is_full() {
            self.buf[self.get_pos() % N].write(item);
            self.len += 1;
            return Ok(());
        } else {
            return Err(item);
        }
    }

    /// Removes the oldest item (slot `head`), or `None` if empty.
    pub(crate) fn pop(&mut self) -> Option<T> {
        if !self.is_empty() {
            // SAFETY: `len > 0`, so slot `head` is initialized. Advancing `head`
            // below moves it out of the initialized range, so it is read once.
            let v = unsafe { self.buf[self.head].assume_init_read() };
            self.head = (self.head + 1) % N;
            self.len -= 1;
            return Some(v);
        }
        return None;
    }
}

impl<T, const N: usize> Drop for Ring<T, N> {
    /// `MaybeUninit` never drops its contents: pop until empty so queued items
    /// are dropped, not leaked.
    fn drop(&mut self) {
        while !self.is_empty() {
            let _ = self.pop();
        }
    }
}

#[cfg(test)]
mod tests;
