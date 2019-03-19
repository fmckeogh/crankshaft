#![no_main]
#![no_std]

extern crate panic_semihosting;
extern crate stm32f1xx_hal as hal;

use core::fmt::Write;
use cortex_m::asm;
use cortex_m_semihosting::hprintln;
use enc28j60::Enc28j60;
use hal::{pac, prelude::*, serial::Serial, spi::Spi};
use rtfm::app;
use smoltcp::wire::*;

const CPU_HZ: u32 = 64_000_000;

#[app(device = stm32f1xx_hal::pac)]
const APP: () = {
    static mut LED: hal::gpio::gpioc::PC13<hal::gpio::Output<hal::gpio::PushPull>> = ();
    static mut SERIAL: hal::serial::Tx<hal::pac::USART1> = ();
    static mut ETH: enc28j60::Enc28j60<
        hal::spi::Spi<
            hal::device::SPI1,
            (
                hal::gpio::gpioa::PA5<hal::gpio::Alternate<hal::gpio::PushPull>>,
                hal::gpio::gpioa::PA6<hal::gpio::Input<hal::gpio::Floating>>,
                hal::gpio::gpioa::PA7<hal::gpio::Alternate<hal::gpio::PushPull>>,
            ),
        >,
        hal::gpio::gpioa::PA4<hal::gpio::Output<hal::gpio::PushPull>>,
        enc28j60::Unconnected,
        hal::gpio::gpioa::PA3<hal::gpio::Output<hal::gpio::PushPull>>,
    > = ();

    #[init]
    fn init() {
        hprintln!("init").unwrap();

        let device: pac::Peripherals = device;
        let _core: rtfm::Peripherals = core;

        let mut rcc = device.RCC.constrain();
        let mut afio = device.AFIO.constrain(&mut rcc.apb2);

        // Clocks
        let clocks = {
            // Power mode
            //device.PWR.cr.modify(|_, w| unsafe { w.vos().bits(0x11) });
            // Flash latency
            device
                .FLASH
                .acr
                .modify(|_, w| unsafe { w.latency().bits(0x11) });

            let mut flash = device.FLASH.constrain();

            rcc.cfgr
                .sysclk(CPU_HZ.hz())
                .pclk1((CPU_HZ / 2).hz())
                .pclk2(CPU_HZ.hz())
                .hclk(CPU_HZ.hz())
                .freeze(&mut flash.acr)
        };

        // GPIO
        let mut gpioa = device.GPIOA.split(&mut rcc.apb2);
        let mut gpiob = device.GPIOB.split(&mut rcc.apb2);
        let mut gpioc = device.GPIOC.split(&mut rcc.apb2);

        // Serial
        let mut serial = {
            let tx = gpiob.pb6.into_alternate_push_pull(&mut gpiob.crl);
            let rx = gpiob.pb7;
            Serial::usart1(
                device.USART1,
                (tx, rx),
                &mut afio.mapr,
                115_200.bps(),
                clocks,
                &mut rcc.apb2,
            )
            .split()
            .0
        };
        writeln!(serial, "serial start").unwrap();

        // LED
        let mut led = gpioc.pc13.into_push_pull_output(&mut gpioc.crh);
        // turn the LED off during initialization
        led.set_high();

        // ENC28J60
        let enc28j60 = {
            // SPI
            let spi = {
                let sck = gpioa.pa5.into_alternate_push_pull(&mut gpioa.crl);
                let miso = gpioa.pa6;
                let mosi = gpioa.pa7.into_alternate_push_pull(&mut gpioa.crl);

                Spi::spi1(
                    device.SPI1,
                    (sck, miso, mosi),
                    &mut afio.mapr,
                    enc28j60::MODE,
                    1.mhz(),
                    clocks,
                    &mut rcc.apb2,
                )
            };

            let mut ncs = gpioa.pa4.into_push_pull_output(&mut gpioa.crl);
            ncs.set_high();
            let mut reset = gpioa.pa3.into_push_pull_output(&mut gpioa.crl);
            reset.set_high();

            let mut delay = AsmDelay {};

            Enc28j60::new(
                spi,
                ncs,
                enc28j60::Unconnected,
                reset,
                &mut delay,
                7 * 1024,
                [0x20, 0x18, 0x03, 0x01, 0x00, 0x00],
            )
            .unwrap()
        };

        LED = led;
        SERIAL = serial;
        ETH = enc28j60;
    }

    #[idle(resources = [LED, SERIAL, ETH])]
    fn idle() -> ! {
        writeln!(resources.SERIAL, "idle").unwrap();

        let mut buf = [0; 256];
        loop {
            let len = resources.ETH.receive(buf.as_mut()).ok().unwrap() as usize;
            let new_buf = &buf[..len];

            match smoltcp::wire::EthernetFrame::new_checked(&new_buf) {
                Ok(frame) => match frame.ethertype() {
                    EthernetProtocol::Arp => {
                        let arp = ArpPacket::new_checked(frame.payload()).unwrap();
                        writeln!(resources.SERIAL, "arp: {:?}", arp).unwrap();
                    }
                    EthernetProtocol::Ipv4 => {
                        writeln!(resources.SERIAL, "ipv4").unwrap();
                    }
                    _ => {
                        unimplemented!();
                    }
                },
                Err(e) => {
                    writeln!(resources.SERIAL, "malformed Ethernet frame, {:?}", e).unwrap();
                }
            }
        }
    }
};

struct AsmDelay {}

impl embedded_hal::blocking::delay::DelayMs<u8> for AsmDelay {
    fn delay_ms(&mut self, ms: u8) {
        asm::delay((ms as u32) * (CPU_HZ / 10));
    }
}
