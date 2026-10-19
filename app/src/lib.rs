//! Board support shared by every demo binary.
#![no_std]

use hal::Clock;
use rp235x_hal as hal;
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
    }
}

/// Any panic, in a task (privileged or not), the kernel or `main`, ends the
/// test with exit 1. Semihosting works unprivileged; a fault (panic-probe's
/// way) would only kill the panicking task now.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    defmt::error!("{}", defmt::Display2Format(info));
    cortex_m_semihosting::debug::exit(cortex_m_semihosting::debug::EXIT_FAILURE);
    loop {}
}

/// End a test binary successfully: `probe-rs run` exits 0.
pub fn exit_ok() -> ! {
    defmt::info!("TEST PASS");
    cortex_m_semihosting::debug::exit(cortex_m_semihosting::debug::EXIT_SUCCESS);
    unreachable!()
}
