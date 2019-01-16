#![no_main]
#![no_std]

extern crate panic_halt;

use cortex_m_semihosting::{hprintln};
use rtfm::app;
use stm32f4xx_hal::{
    gpio::{gpiod::PD13, Output, PushPull},
    prelude::*,
    stm32::{self as device, EXTI},
};

const CLOCK_MHZ: u32 = 90;
const PERIOD: u32 = 10_000_000;

#[app(device = stm32f4xx_hal::stm32)]
const APP: () = {
    static mut ON: bool = false;
    static mut LED: PD13<Output<PushPull>> = ();
    static mut EXTI: EXTI = ();

    #[init(spawn = [blinky])]
    fn init() {
        hprintln!("init").unwrap();

        let device: device::Peripherals = device;

        device.PWR.cr.modify(|_, w| unsafe { w.vos().bits(0x11) });
        device
            .FLASH
            .acr
            .modify(|_, w| unsafe { w.latency().bits(0x11) });

        let rcc = device.RCC.constrain();

        let _clocks = rcc
            .cfgr
            .sysclk(CLOCK_MHZ.mhz())
            .pclk1((CLOCK_MHZ / 2).mhz())
            .pclk2(CLOCK_MHZ.mhz())
            .hclk(CLOCK_MHZ.mhz())
            .freeze();

        let gpioa = device.GPIOA.split();
        let _gpiob = device.GPIOB.split();
        let _gpioc = device.GPIOC.split();
        let gpiod = device.GPIOD.split();

        let led = gpiod.pd13.into_push_pull_output();

        gpioa.pa0.into_floating_input();
        device
            .SYSCFG
            .exticr1
            .modify(|_, w| unsafe { w.exti0().bits(0x01) });
        // Enable interrupt on EXTI0
        device.EXTI.imr.modify(|_, w| w.mr0().set_bit());
        // Set falling trigger selection for EXTI0
        device.EXTI.ftsr.modify(|_, w| w.tr0().set_bit());

        spawn.blinky().unwrap();

        LED = led;
        EXTI = device.EXTI;
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

        schedule.blinky(scheduled + PERIOD.cycles()).unwrap();
    }

    #[interrupt(resources = [EXTI])]
    fn EXTI0() {
        hprintln!("interrupt").unwrap();
        resources.EXTI.pr.modify(|_, w| w.pr0().set_bit());
    }

    extern "C" {
        fn SPI1();
    }
};
