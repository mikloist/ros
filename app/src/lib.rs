//! Board support shared by every demo binary.
#![no_std]

use hal::Clock;
pub use rp235x_hal as hal;
// `kernel` provides the critical-section impl every binary needs (defmt, HAL).
use {defmt_rtt as _, kernel as _};

pub use hal::entry;

/// Tells the RP2350 boot ROM this is a Secure Arm executable.
#[unsafe(link_section = ".start_block")]
#[used]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

/// Pico 2 W crystal.
pub const XTAL_FREQ_HZ: u32 = 12_000_000;

pub struct Board {
    pub pins: hal::gpio::Pins,
    pub timer: hal::Timer<hal::timer::CopyableTimer0>,
    /// Core clock in Hz (150 MHz); `kernel::start` derives the 1 kHz tick from it.
    pub sys_hz: u32,
    psm: hal::pac::PSM,
    ppb: hal::pac::PPB,
    fifo: hal::sio::SioFifo,
}

/// Core 1's own stack for exceptions (MSP). In `.stacks`, so no task's MPU
/// regions cover it (shared RAM would let every task overwrite it).
#[unsafe(link_section = ".stacks")]
static CORE1_STACK: hal::multicore::Stack<1024> = hal::multicore::Stack::new();

impl Board {
    /// Starts core 1 in `kernel::start_secondary`: it waits for
    /// `kernel::start` on core 0, then both cores run tasks. Call before
    /// `kernel::start`; without it the kernel runs on core 0 alone.
    pub fn start_core1(&mut self) {
        let mut mc = hal::multicore::Multicore::new(&mut self.psm, &mut self.ppb, &mut self.fifo);
        let stack = CORE1_STACK.take().expect("core 1 already started");
        mc.cores()[1].spawn(stack, || kernel::start_secondary()).unwrap();
    }
}

/// Clocks to 150 MHz, GPIO bank, TIMER0.
pub fn init() -> Board {
    let mut pac = hal::pac::Peripherals::take().unwrap();
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);
    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .unwrap();
    let timer = hal::Timer::new_timer0(pac.TIMER0, &mut pac.RESETS, &clocks);
    let sio = hal::Sio::new(pac.SIO);
    let fifo = sio.fifo;
    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );
    Board {
        pins,
        timer,
        sys_hz: clocks.system_clock.freq().to_Hz(),
        psm: pac.PSM,
        ppb: pac.PPB,
        fifo,
    }
}

/// Any panic, in a task (privileged or not), the kernel or `main`, ends the
/// test with exit 1. Semihosting works unprivileged; a fault (panic-probe's
/// way) would only kill the panicking task now.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    defmt::error!("{}", defmt::Display2Format(info));
    kernel::exit(false)
}

/// End a test binary successfully: `probe-rs run` exits 0.
pub fn exit_ok() -> ! {
    defmt::info!("TEST PASS");
    kernel::exit(true)
}
