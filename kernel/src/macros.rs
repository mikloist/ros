//! Declaring kernel objects. Each macro puts the object in the linker section
//! the MPU setup expects; `kernel::start` and the syscalls refuse objects
//! found anywhere else.

/// A task stack of `$words` 32-bit words (a multiple of 8), in `.stacks`.
///
/// `kernel::stack!(STACK_A, 512);`
#[macro_export]
macro_rules! stack {
    ($vis:vis $name:ident, $words:expr) => {
        #[unsafe(link_section = ".stacks")]
        $vis static $name: $crate::Stack<$words> = $crate::Stack::new();
    };
}

/// A counting semaphore in `.kobj`.
///
/// `kernel::semaphore!(DONE, 0);`
#[macro_export]
macro_rules! semaphore {
    ($vis:vis $name:ident, $count:expr) => {
        #[unsafe(link_section = ".kobj")]
        $vis static $name: $crate::Semaphore = $crate::Semaphore::new($count);
    };
}

/// A queue of up to `$n` items of type `$t`, in `.kobj`.
///
/// `kernel::queue!(Q: u32, 4);`
#[macro_export]
macro_rules! queue {
    ($vis:vis $name:ident: $t:ty, $n:expr) => {
        #[unsafe(link_section = ".kobj")]
        $vis static $name: $crate::Queue<$t, $n> = $crate::Queue::new();
    };
}

/// A mutex around a `$t`: lock state in `.kobj`, data in shared RAM.
///
/// `kernel::mutex!(COUNTER: u32 = 0);`
#[macro_export]
macro_rules! mutex {
    ($vis:vis $name:ident: $t:ty = $init:expr) => {
        $vis static $name: $crate::Mutex<$t> = {
            #[unsafe(link_section = ".kobj")]
            static CORE: $crate::RawMutex = $crate::RawMutex::new();
            static DATA: $crate::MutexData<$t> = $crate::MutexData::new($init);
            $crate::Mutex::from_parts(&CORE, &DATA)
        };
    };
}
