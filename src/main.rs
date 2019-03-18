#![no_main]
#![no_std]

extern crate panic_semihosting;
extern crate stm32f1xx_hal as hal;

use cortex_m::asm;
use cortex_m_semihosting::hprintln;
use enc28j60::Enc28j60;
use hal::{pac, prelude::*, spi::Spi};
use heapless::{consts::*, FnvIndexMap};
use jnet::{arp, ether, icmp, ipv4, mac, udp, Buffer};
use rtfm::app;

const MAC: mac::Addr = mac::Addr([0x20, 0x18, 0x03, 0x01, 0x00, 0x00]);
const IP: ipv4::Addr = ipv4::Addr([192, 168, 1, 2]);

#[app(device = stm32f1xx_hal::pac)]
const APP: () = {
    static mut LED: hal::gpio::gpioc::PC13<hal::gpio::Output<hal::gpio::PushPull>> = ();
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
        let mut core: rtfm::Peripherals = core;

        let mut flash = device.FLASH.constrain();
        let mut rcc = device.RCC.constrain();
        let clocks = rcc.cfgr.freeze(&mut flash.acr);
        let mut afio = device.AFIO.constrain(&mut rcc.apb2);

        // GPIO
        let mut gpioa = device.GPIOA.split(&mut rcc.apb2);
        let _gpiob = device.GPIOB.split(&mut rcc.apb2);
        let mut gpioc = device.GPIOC.split(&mut rcc.apb2);

        // LED
        let mut led = gpioc.pc13.into_push_pull_output(&mut gpioc.crh);
        // turn the LED off during initialization
        led.set_high();

        // ENC28J60
        let mut enc28j60 = {
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
        ETH = enc28j60;
    }

    #[idle(resources = [LED, ETH])]
    fn idle() -> ! {
        hprintln!("idle").unwrap();

        let mut cache = FnvIndexMap::<_, _, U8>::new();

        let mut buf = [0; 256];
        loop {
            let mut buf = Buffer::new(&mut buf);
            let len = resources.ETH.receive(buf.as_mut()).ok().unwrap();
            buf.truncate(len);

            if let Ok(mut eth) = ether::Frame::parse(buf) {
                //hprintln!("\nRx({})", eth.as_bytes().len());
                //hprintln!("* {:?}", eth);

                let src_mac = eth.get_source();

                match eth.get_type() {
                    ether::Type::Arp => {
                        if let Ok(arp) = arp::Packet::parse(eth.payload_mut()) {
                            match arp.downcast() {
                                Ok(mut arp) => {
                                    //hprintln!("** {:?}", arp);

                                    if !arp.is_a_probe() {
                                        cache.insert(arp.get_spa(), arp.get_sha()).ok();
                                    }

                                    // are they asking for us?
                                    if arp.get_oper() == arp::Operation::Request
                                        && arp.get_tpa() == IP
                                    {
                                        // reply to the ARP request
                                        let tha = arp.get_sha();
                                        let tpa = arp.get_spa();

                                        arp.set_oper(arp::Operation::Reply);
                                        arp.set_sha(MAC);
                                        arp.set_spa(IP);
                                        arp.set_tha(tha);
                                        arp.set_tpa(tpa);
                                        //hprintln!("\n** {:?}", arp);
                                        let arp_len = arp.len();

                                        // update the Ethernet header
                                        eth.set_destination(tha);
                                        eth.set_source(MAC);
                                        eth.truncate(arp_len);
                                        //hprintln!("* {:?}", eth);

                                        resources.LED.toggle();

                                        //hprintln!("Tx({})", eth.as_bytes().len());
                                        resources.ETH.transmit(eth.as_bytes()).ok().unwrap();
                                    }
                                }
                                Err(_arp) => {
                                    // Not a Ethernet/IPv4 ARP packet
                                    //hprintln!("** {:?}", _arp);
                                }
                            }
                        } else {
                            // malformed ARP packet
                            //hprintln!("Err(A)");
                        }
                    }
                    ether::Type::Ipv4 => {
                        if let Ok(mut ip) = ipv4::Packet::parse(eth.payload_mut()) {
                            //hprintln!("** {:?}", ip);

                            let src_ip = ip.get_source();

                            if !src_mac.is_broadcast() {
                                cache.insert(src_ip, src_mac).ok();
                            }

                            match ip.get_protocol() {
                                ipv4::Protocol::Icmp => {
                                    if let Ok(icmp) = icmp::Packet::parse(ip.payload_mut()) {
                                        match icmp.downcast::<icmp::EchoRequest>() {
                                            Ok(request) => {
                                                // is an echo request
                                                //hprintln!("*** {:?}", request);

                                                let src_mac = cache
                                                    .get(&src_ip)
                                                    .unwrap_or_else(|| unimplemented!());

                                                let _reply: icmp::Packet<_, icmp::EchoReply, _> =
                                                    request.into();
                                                //hprintln!("\n*** {:?}", _reply);

                                                // update the IP header
                                                let mut ip = ip.set_source(IP);
                                                ip.set_destination(src_ip);
                                                let _ip = ip.update_checksum();
                                                //hprintln!("** {:?}", _ip);

                                                // update the Ethernet header
                                                eth.set_destination(*src_mac);
                                                eth.set_source(MAC);
                                                //hprintln!("* {:?}", eth);

                                                resources.LED.toggle();
                                                //hprintln!("Tx({})", eth.as_bytes().len());
                                                resources
                                                    .ETH
                                                    .transmit(eth.as_bytes())
                                                    .ok()
                                                    .unwrap();
                                            }
                                            Err(_icmp) => {
                                                //hprintln!("*** {:?}", _icmp);
                                            }
                                        }
                                    } else {
                                        // Malformed ICMP packet
                                        //hprintln!("Err(B)");
                                    }
                                }
                                ipv4::Protocol::Udp => {
                                    if let Ok(mut udp) = udp::Packet::parse(ip.payload_mut()) {
                                        //hprintln!("*** {:?}", udp);

                                        if let Some(src_mac) = cache.get(&src_ip) {
                                            let src_port = udp.get_source();
                                            let dst_port = udp.get_destination();

                                            // update the UDP header
                                            udp.set_source(dst_port);
                                            udp.set_destination(src_port);
                                            udp.zero_checksum();
                                            //hprintln!("\n*** {:?}", udp);

                                            // update the IP header
                                            let mut ip = ip.set_source(IP);
                                            ip.set_destination(src_ip);
                                            let ip = ip.update_checksum();
                                            let ip_len = ip.len();
                                            //hprintln!("** {:?}", ip);

                                            // update the Ethernet header
                                            eth.set_destination(*src_mac);
                                            eth.set_source(MAC);
                                            eth.truncate(ip_len);
                                            //hprintln!("* {:?}", eth);

                                            resources.LED.toggle();
                                            //hprintln!("Tx({})", eth.as_bytes().len());
                                            resources.ETH.transmit(eth.as_bytes()).ok().unwrap();
                                        }
                                    } else {
                                        // malformed UDP packet
                                        //hprintln!("Err(C)");
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            // malformed IPv4 packet
                            //hprintln!("Err(D)");
                        }
                    }
                    _ => {}
                }
            } else {
                // malformed Ethernet frame
                //hprintln!("Err(E)");
            }
        }
    }
};

struct AsmDelay {}

impl embedded_hal::blocking::delay::DelayMs<u8> for AsmDelay {
    fn delay_ms(&mut self, ms: u8) {
        asm::delay((ms as u32) * (8_000_000 / 10));
    }
}
