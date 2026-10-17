//! M3: semaphore. Checks timeout, try-take, counting, and interrupt -> task:
//! a TIMER0 alarm interrupt `give`s every 3 ms; a task blocked in `take` must
//! wake within microseconds of each give.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering::Relaxed};
use kernel::{Duration, Semaphore, Stack, Task, Timeout};
use rp235x_hal as hal;
use hal::fugit::MicrosDurationU32;
use hal::pac::interrupt;
use hal::timer::Alarm;

type Timer = hal::Timer<hal::timer::CopyableTimer0>;
type Alarm0 = hal::timer::Alarm0<hal::timer::CopyableTimer0>;

const PERIOD: MicrosDurationU32 = MicrosDurationU32::millis(3);
const ROUNDS: u32 = 20;

static NEVER: Semaphore = Semaphore::new(0);
static COUNTED: Semaphore = Semaphore::new(0);
static IRQ: Semaphore = Semaphore::new(0);
/// TIMER0 low word at the moment the ISR gave `IRQ`.
static GIVEN_AT: AtomicU32 = AtomicU32::new(0);

static mut TIMER: Option<Timer> = None;
static mut ALARM: Option<Alarm0> = None;
static STACK: Stack<1024> = Stack::new();

fn micros() -> u32 {
    // SAFETY: written once in `main` before the kernel starts; read-only afterwards.
    unsafe { (*&raw const TIMER).unwrap() }.get_counter_low()
}

#[interrupt]
fn TIMER0_IRQ_0() {
    // SAFETY: only this handler touches ALARM after `main` armed it.
    let alarm = unsafe { (*&raw mut ALARM).as_mut().unwrap() };
    alarm.clear_interrupt();
    alarm.schedule(PERIOD).unwrap();
    GIVEN_AT.store(micros(), Relaxed);
    IRQ.give();
}

fn waiter() -> ! {
    // Timeout: nobody ever gives NEVER.
    let t0 = kernel::now();
    assert_eq!(NEVER.take(Some(Duration::from_millis(5))), Err(Timeout));
    assert_eq!(kernel::now() - t0, Duration::from_millis(5), "timed take did not last 5 ticks");
    assert_eq!(NEVER.take(Some(Duration::ZERO)), Err(Timeout), "try-take blocked or succeeded");
    defmt::info!("timeout ok");

    // Counting: gives with nobody waiting accumulate.
    COUNTED.give();
    COUNTED.give();
    assert_eq!(COUNTED.take(Some(Duration::ZERO)), Ok(()));
    assert_eq!(COUNTED.take(Some(Duration::ZERO)), Ok(()));
    assert_eq!(COUNTED.take(Some(Duration::ZERO)), Err(Timeout), "count went above 2");
    defmt::info!("counting ok");

    // Interrupt -> task.
    // SAFETY: the alarm is armed before its interrupt is unmasked.
    unsafe {
        let alarm = (*&raw mut ALARM).as_mut().unwrap();
        alarm.enable_interrupt();
        alarm.schedule(PERIOD).unwrap();
        cortex_m::peripheral::NVIC::unmask(hal::pac::Interrupt::TIMER0_IRQ_0);
    }
    let mut worst = 0;
    for _ in 0..ROUNDS {
        assert_eq!(IRQ.take(Some(Duration::from_millis(10))), Ok(()), "interrupt give never arrived");
        let latency = micros().wrapping_sub(GIVEN_AT.load(Relaxed));
        worst = worst.max(latency);
    }
    cortex_m::peripheral::NVIC::mask(hal::pac::Interrupt::TIMER0_IRQ_0);
    defmt::info!("irq -> task: {} wakeups, worst latency {} us", ROUNDS, worst);
    assert!(worst < 50, "task woke {} us after the give", worst);
    app::exit_ok()
}

#[app::entry]
fn main() -> ! {
    let mut b = app::init();
    // SAFETY: no task or interrupt is running yet.
    unsafe {
        *&raw mut ALARM = b.timer.alarm_0();
        *&raw mut TIMER = Some(b.timer);
    }
    let mut tasks = [Task::new("waiter", 2, waiter, &STACK)];
    kernel::start(&mut tasks, b.sys_hz)
}
