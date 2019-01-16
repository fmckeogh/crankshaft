#![no_main]
#![no_std]

extern crate panic_halt;

use stm32f4xx_hal::{
    prelude::*,
    stm32 as device,
    gpio::{gpiod::PD13, Output, PushPull}
};
use cortex_m_semihosting::{debug, hprintln};
use rtfm::app;

/*
#[entry]
fn main() -> ! {
    hprintln!("init").unwrap();

    let dp = device::Peripherals::take().unwrap();

    let mut rcc = dp.RCC.constrain();

    let mut gpiod = dp.GPIOD.split();

    let mut led = gpiod.pd13.into_push_pull_output();
    
    let mut delay = Delay::new(cp.SYST, clocks);

    led.set_high();

    hprintln!("blinking...").unwrap();

    loop {
        led.set_high();
        delay.delay_ms(1_000_u16);
        led.set_low();
        delay.delay_ms(1_000_u16);
    }
}
*/

const PERIOD: u32 = 8_000_000;

#[app(device = stm32f4xx_hal::stm32)]
const APP: () = {
    static mut ON: bool = false;
    static mut LED: PD13<Output<PushPull>> = ();

    #[init(spawn = [blinky])]
    fn init() {
        hprintln!("init").unwrap();

        let device: device::Peripherals = device;

        let mut gpioa = device.GPIOA.split();
        let mut gpiob = device.GPIOB.split();
        let mut gpioc = device.GPIOC.split();
        let mut gpiod = device.GPIOD.split();

        let mut led = gpiod.pd13.into_push_pull_output();

        spawn.blinky().unwrap();

        LED = led;
    }

    #[idle]
    fn idle() -> ! {
        hprintln!("idle").unwrap();

        loop {}
    }

    #[task(schedule = [blinky], resources = [ON, LED])]
    fn blinky() {
        match *resources.ON {
            true => resources.LED.set_high(),
            false => resources.LED.set_low(),
        }
        *resources.ON ^= true;

        schedule
            .blinky(scheduled + PERIOD.cycles())
            .unwrap();
    }

    extern "C" {
        fn SPI1();
    }
};