#![no_main]
#![no_std]
#![feature(alloc)]
#![feature(lang_items)]

#[macro_use]
extern crate alloc;
extern crate panic_halt;

use alloc_cortex_m::CortexMHeap;
use cortex_m_semihosting::hprintln;
use embedded_graphics::{fonts::Font6x8, prelude::*};
use rtfm::app;
use ssd1306::interface::I2cInterface;
use ssd1306::{prelude::*, Builder};
use stm32f4xx_hal::{
    gpio::{
        gpioa::PA8,
        gpioc::PC9,
        gpiod::{PD12, PD13, PD14, PD15},
        Alternate, Output, PushPull, AF4,
    },
    i2c::I2c,
    prelude::*,
    stm32::{self as device, EXTI, I2C3, TIM11},
};

const CPU_MHZ: u32 = 90;
const PERIOD: u32 = 5_000_000;

#[global_allocator]
static ALLOCATOR: CortexMHeap = CortexMHeap::empty();

#[app(device = stm32f4xx_hal::stm32)]
const APP: () = {
    static mut DISPLAY: GraphicsMode<
        I2cInterface<I2c<I2C3, (PA8<Alternate<AF4>>, PC9<Alternate<AF4>>)>>,
    > = ();
    static mut LED_GREEN: PD12<Output<PushPull>> = ();
    static mut LED_ORANGE: PD13<Output<PushPull>> = ();
    static mut LED_RED: PD14<Output<PushPull>> = ();
    static mut LED_BLUE: PD15<Output<PushPull>> = ();

    static mut EXTI: EXTI = ();
    static mut TIM11: TIM11 = ();

    static mut VAL: u8 = 0;

    #[init(spawn = [display_update])]
    fn init() {
        hprintln!("init").unwrap();

        // Allocator
        {
            let start = cortex_m_rt::heap_start() as usize;
            let size = 512; // in bytes
            unsafe { ALLOCATOR.init(start, size) }
        }

        let device: device::Peripherals = device;

        let clocks = {
            device.PWR.cr.modify(|_, w| unsafe { w.vos().bits(0x11) });
            device
                .FLASH
                .acr
                .modify(|_, w| unsafe { w.latency().bits(0x11) });

            device.RCC.apb2enr.modify(|_, w| w.tim11en().set_bit());
            device.TIM11.psc.modify(|_, w| unsafe { w.psc().bits(64) });

            let rcc = device.RCC.constrain();
            rcc.cfgr
                .sysclk(CPU_MHZ.mhz())
                .pclk1((CPU_MHZ / 2).mhz())
                .pclk2(CPU_MHZ.mhz())
                .hclk(CPU_MHZ.mhz())
                .freeze()
        };

        let gpioa = device.GPIOA.split();
        let _gpiob = device.GPIOB.split();
        let gpioc = device.GPIOC.split();
        let gpiod = device.GPIOD.split();

        let mut green = gpiod.pd12.into_push_pull_output();
        let orange = gpiod.pd13.into_push_pull_output();
        let red = gpiod.pd14.into_push_pull_output();
        let blue = gpiod.pd15.into_push_pull_output();

        green.set_high();

        // Enable interrupt on PA0 and PA1
        {
            gpioa.pa0.into_floating_input();
            gpioa.pa2.into_floating_input();
            gpioa.pa3.into_floating_input();
            device.SYSCFG.exticr1.modify(|_, w| unsafe {
                w.exti0().bits(0x00).exti2().bits(0x00).exti3().bits(0x00)
            });
            // Enable interrupt on EXTI0 and 3
            device
                .EXTI
                .imr
                .modify(|_, w| w.mr0().set_bit().mr2().set_bit().mr3().set_bit());
            // Set falling trigger selection for EXTI0 and EXTI3
            device
                .EXTI
                .ftsr
                .modify(|_, w| w.tr0().set_bit().tr3().set_bit());
            // Set rising trigger selection for EXTI2
            device.EXTI.rtsr.modify(|_, w| w.tr2().set_bit());
        };

        // Display setup
        let display = {
            let sda = gpioc.pc9.into_alternate_af4();
            let scl = gpioa.pa8.into_alternate_af4();
            let i2c = I2c::i2c3(device.I2C3, (scl, sda), 600.khz(), clocks);

            let mut display: GraphicsMode<_> = Builder::new().connect_i2c(i2c).into();

            display.init().unwrap();
            display.clear();
            display.flush().unwrap();

            display
        };

        spawn.display_update().unwrap();

        DISPLAY = display;
        LED_GREEN = green;
        LED_ORANGE = orange;
        LED_RED = red;
        LED_BLUE = blue;

        EXTI = device.EXTI;
        TIM11 = device.TIM11;
    }

    #[idle]
    fn idle() -> ! {
        hprintln!("idle").unwrap();

        loop {}
    }

    #[task(priority = 2, schedule = [display_update], resources = [DISPLAY, VAL])]
    fn display_update() {
        let mut val = 0;

        resources.VAL.lock(|v| {
            val = *v;
        });

        resources.DISPLAY.clear();
        resources
            .DISPLAY
            .draw(Font6x8::render_str(&format!("val: {}%", val)).into_iter());
        resources.DISPLAY.flush().unwrap();

        schedule
            .display_update(scheduled + PERIOD.cycles())
            .unwrap();
    }

    /*
    #[interrupt(priority = 3, resources = [EXTI, VAL])]
    fn EXTI0() {
        *resources.VAL += 1;
        resources.EXTI.pr.modify(|_, w| w.pr0().set_bit());
    }
    */

    #[interrupt(priority = 3, resources = [EXTI, TIM11])]
    fn EXTI2() {
        // start timer
        resources.TIM11.cr1.modify(|_, w| w.cen().enabled());

        resources.EXTI.pr.modify(|_, w| w.pr2().set_bit());
    }

    #[interrupt(priority = 3, resources = [EXTI, TIM11, VAL])]
    fn EXTI3() {
        // stop timer and update val
        resources.TIM11.cr1.modify(|_, w| w.cen().disabled());
        let raw = resources.TIM11.cnt.read().cnt().bits();
        resources.TIM11.cnt.write(|w| unsafe { w.cnt().bits(0) });

        *resources.VAL = ((raw as f32 - 846.0) / 16.28) as u8;

        resources.EXTI.pr.modify(|_, w| w.pr3().set_bit());
    }

    extern "C" {
        fn SPI1();
    }
};

#[lang = "oom"]
#[no_mangle]
pub fn rust_oom(layout: core::alloc::Layout) -> ! {
    panic!("{:?}", layout);
}
