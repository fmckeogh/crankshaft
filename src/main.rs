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
    gpio::{gpioa::PA8, gpioc::PC9, gpiod::PD14, Alternate, Output, PushPull, AF4},
    i2c::I2c,
    prelude::*,
    stm32::{self as device, EXTI, I2C3, TIM11, TIM4},
};

const CPU_HZ: u32 = 100_000_000;
const PERIOD: u32 = 5_000_000;

#[global_allocator]
static ALLOCATOR: CortexMHeap = CortexMHeap::empty();

#[app(device = stm32f4xx_hal::stm32)]
const APP: () = {
    static mut DISPLAY: GraphicsMode<
        I2cInterface<I2c<I2C3, (PA8<Alternate<AF4>>, PC9<Alternate<AF4>>)>>,
    > = ();
    static mut LED_RED: PD14<Output<PushPull>> = ();

    static mut EXTI: EXTI = ();
    static mut TIM4: TIM4 = ();
    static mut TIM11: TIM11 = ();

    static mut VAL: u8 = 0;

    #[init(spawn = [display_update])]
    fn init() {
        hprintln!("init...").unwrap();

        // Allocator
        {
            let start = cortex_m_rt::heap_start() as usize;
            let size = 512; // in bytes
            unsafe { ALLOCATOR.init(start, size) }
        }

        let device: device::Peripherals = device;

        // GPIO
        let gpioa = device.GPIOA.split();
        let _gpiob = device.GPIOB.split();
        let gpioc = device.GPIOC.split();
        let gpiod = device.GPIOD.split();

        // Enable TIM4 and TIM11
        {
            device.RCC.apb1enr.modify(|_, w| w.tim4en().set_bit());
            device.RCC.apb2enr.modify(|_, w| w.tim11en().set_bit());
        }

        // Set TIM11 prescaler to 64
        device.TIM11.psc.modify(|_, w| unsafe { w.psc().bits(64) });

        // Set core speed
        let clocks = {
            // Power mode
            device.PWR.cr.modify(|_, w| unsafe { w.vos().bits(0x11) });
            // Flash latency
            device
                .FLASH
                .acr
                .modify(|_, w| unsafe { w.latency().bits(0x11) });

            let rcc = device.RCC.constrain();
            rcc.cfgr
                .sysclk(CPU_HZ.hz())
                .pclk1((CPU_HZ / 2).hz())
                .pclk2(CPU_HZ.hz())
                .hclk(CPU_HZ.hz())
                .freeze()
        };

        // PWM outputs
        {
            gpiod.pd12.into_alternate_af2();
            gpiod.pd13.into_alternate_af2();
            gpiod.pd15.into_alternate_af2();

            // Set prescaler to 1
            device.TIM4.psc.modify(|_, w| unsafe { w.psc().bits(1) });

            device.TIM4.arr.modify(|_, w| w.arr().bits(100));
            device
                .TIM4
                .cr1
                .modify(|_, w| unsafe { w.dir().up().ckd().bits(1).arpe().set_bit() });
            device.TIM4.ccmr1_output.modify(|_, w| unsafe {
                w.oc1m()
                    .bits(0b111)
                    .oc1pe()
                    .set_bit()
                    .oc2m()
                    .bits(0b111)
                    .oc2pe()
                    .set_bit()
            });
            device
                .TIM4
                .ccmr2_output
                .modify(|_, w| unsafe { w.oc4m().bits(0b111).oc4pe().set_bit() });
            device.TIM4.egr.write(|w| w.ug().set_bit());
            device.TIM4.ccer.modify(|_, w| {
                w.cc1p()
                    .bit(true)
                    .cc1e()
                    .set_bit()
                    .cc2p()
                    .bit(true)
                    .cc2e()
                    .set_bit()
                    .cc4p()
                    .bit(true)
                    .cc4e()
                    .set_bit()
            });
            device.TIM4.cr1.modify(|_, w| w.cen().enabled());
        }

        // Enable interrupts on PA0, PA2 and PA3
        {
            // User pushbutton connected to PA0
            gpioa.pa0.into_floating_input();
            // RC_IN connected to both PA2 and PA3
            gpioa.pa2.into_floating_input();
            gpioa.pa3.into_floating_input();

            // Set to GPIOA on all EXTI0/2/3
            device.SYSCFG.exticr1.modify(|_, w| unsafe {
                w.exti0().bits(0x00).exti2().bits(0x00).exti3().bits(0x00)
            });
            // Enable interrupts on EXTI0/2/3
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
            let i2c = I2c::i2c3(device.I2C3, (scl, sda), 1000.khz(), clocks);

            let mut display: GraphicsMode<_> = Builder::new().connect_i2c(i2c).into();

            display.init().unwrap();
            display.clear();
            display.flush().unwrap();

            display
        };

        spawn.display_update().unwrap();

        hprintln!("init complete").unwrap();

        DISPLAY = display;
        LED_RED = gpiod.pd14.into_push_pull_output();

        EXTI = device.EXTI;
        TIM4 = device.TIM4;
        TIM11 = device.TIM11;
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
        // Start timer
        resources.TIM11.cr1.modify(|_, w| w.cen().enabled());

        resources.EXTI.pr.modify(|_, w| w.pr2().set_bit());
    }

    #[interrupt(priority = 3, resources = [EXTI, TIM4, TIM11, VAL])]
    fn EXTI3() {
        // Stop timer and update val
        resources.TIM11.cr1.modify(|_, w| w.cen().disabled());
        let raw = resources.TIM11.cnt.read().cnt().bits();
        // Reset counter - probably a better way to do this
        resources.TIM11.cnt.write(|w| unsafe { w.cnt().bits(0) });

        // Calculate val
        let val = {
            let max = 2755.0;
            let min = 941.0;
            let range = max - min;
            (((raw as f32 - min) * 100.0) / range) as u8
        };

        // Write to PWM duty cycle registers and global variable
        resources.TIM4.ccr1.modify(|_, w| w.ccr1().bits(val.into()));
        resources.TIM4.ccr2.modify(|_, w| w.ccr2().bits(val.into()));
        resources.TIM4.ccr4.modify(|_, w| w.ccr4().bits(val.into()));
        *resources.VAL = val;

        // Clear flag
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
