//! ENC28J60 demo: a RESTful LED using CoAP
//!
//! The server will expose the LED as a resource under the `/led` path. You can use the CoAP client
//! in the [`jnet`] crate to interact with the server.
//!
//! - `coap GET coap://192.168.1.33/led` will return the state of the LED: either "on" or "off".
//! - `coap PUT coap://192.168.1.33/led on` will change the state of the LED; the payload must be
//!   either "on" or "off".
//!
//! [`jnet`]: https://github.com/japaric/jnet

#![feature(nll)]
#![no_main]
#![no_std]

#[macro_use]
extern crate cortex_m;
extern crate cortex_m_rt as rt;
#[macro_use]
extern crate serde_derive;
extern crate serde_json_core as json;
extern crate stm32f4xx_hal as hal;
#[macro_use]
extern crate panic_itm;

use core::convert::TryInto;
use cortex_m::asm;
use embedded_hal::digital::StatefulOutputPin;
use enc28j60::{Enc28j60, NextPacket};
use hal::prelude::*;
use hal::spi::Spi;
use hal::stm32 as pac;
use heapless::consts::*;
use heapless::FnvIndexMap;
use jnet::{arp, coap, ether, icmp, ipv4, mac, udp, Buffer};
use rt::{entry, exception, ExceptionFrame};
use rtfm::app;

/* Constants */
const KB: u16 = 1024;

/* Network configuration */
const MAC: mac::Addr = mac::Addr([0x20, 0x18, 0x03, 0x01, 0x00, 0x00]);
const IP: ipv4::Addr = ipv4::Addr([192, 168, 1, 2]);

// LED resource
#[derive(Deserialize, Serialize)]
struct Led {
    led: bool,
}

const CPU_HZ: u32 = 50_000_000;

#[app(device = stm32f4xx_hal::stm32)]
const APP: () = {
    static mut ITM: cortex_m::peripheral::ITM = ();
    static mut EXTI: pac::EXTI = ();
    static mut LED: hal::gpio::gpiod::PD14<hal::gpio::Output<hal::gpio::PushPull>> = ();
    static mut ETH: enc28j60::Enc28j60<
        hal::spi::Spi<
            hal::stm32::SPI1,
            (
                hal::gpio::gpioa::PA5<hal::gpio::Alternate<hal::gpio::AF5>>,
                hal::gpio::gpioa::PA6<hal::gpio::Alternate<hal::gpio::AF5>>,
                hal::gpio::gpioa::PA7<hal::gpio::Alternate<hal::gpio::AF5>>,
            ),
        >,
        hal::gpio::gpioa::PA4<hal::gpio::Output<hal::gpio::PushPull>>,
        hal::gpio::gpioa::PA0<hal::gpio::Input<hal::gpio::Floating>>,
        hal::gpio::gpioa::PA3<hal::gpio::Output<hal::gpio::PushPull>>,
    > = ();

    static mut CACHE: FnvIndexMap<jnet::ipv4::Addr, jnet::mac::Addr, U8> = ();

    #[init]
    fn init() {
        let mut core: rtfm::Peripherals = core;
        let device: pac::Peripherals = device;
        let mut gpioa = device.GPIOA.split();
        let mut gpiod = device.GPIOD.split();

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

        // Enable interrupts
        // User enc28j60 INT connected to PA0
        let int = gpioa.pa0.into_floating_input();
        {
            // Set EXTI0 to GPIOA
            device
                .SYSCFG
                .exticr1
                .modify(|_, w| unsafe { w.exti0().bits(0x00) });

            // Enable interrupts on EXTI0
            device.EXTI.imr.modify(|_, w| w.mr0().set_bit());

            // Set falling trigger selection for EXTI0
            device.EXTI.ftsr.modify(|_, w| w.tr0().set_bit());
        }

        let mut itm = core.ITM;
        let mut stim = &mut itm.stim[0];
        iprintln!(stim, "\ninit start");

        // turn the LED off during initialization
        let mut led = gpiod.pd14.into_push_pull_output();
        led.set_low();

        // Ethernet
        let mut eth = {
            let mut rst = gpioa.pa3.into_push_pull_output();
            rst.set_high();
            let mut ncs = gpioa.pa4.into_push_pull_output();
            ncs.set_high();

            let spi = {
                let sck = gpioa.pa5.into_alternate_af5();
                let miso = gpioa.pa6.into_alternate_af5();
                let mosi = gpioa.pa7.into_alternate_af5();

                Spi::spi1(
                    device.SPI1,
                    (sck, miso, mosi),
                    enc28j60::MODE,
                    8_000_000.hz(),
                    clocks,
                )
            };

            let mut delay = AsmDelay {};

            Enc28j60::new(spi, ncs, int, rst, &mut delay, 7 * KB, MAC.0).unwrap()
        };

        iprintln!(stim, "init done");

        ITM = itm;
        EXTI = device.EXTI;
        LED = led;
        ETH = eth;

        CACHE = FnvIndexMap::<ipv4::Addr, mac::Addr, U8>::new();
    }

    #[interrupt(resources = [ITM, EXTI, LED, ETH, CACHE])]
    fn EXTI0() {
        let _stim = &mut resources.ITM.stim[0];
        iprintln!(_stim, "EXTI0");

        let mut buf = [0u8; 128];
        match resources.ETH.next_packet() {
            Ok(Some(packet)) => {
                if packet.len() > 128 {
                    panic!("too big");
                    //packet.ignore().unwrap();
                }

                let buf = packet.read(&mut buf[..]).unwrap();

                if let Ok(mut eth) = ether::Frame::parse(buf) {
                    iprintln!(_stim, "\nRx({})", eth.as_bytes().len());
                    iprintln!(_stim, "* {:?}", eth);

                    let mac_src = eth.get_source();

                    match eth.get_type() {
                        ether::Type::Arp => {
                            if let Ok(arp) = arp::Packet::parse(eth.payload_mut()) {
                                match arp.downcast() {
                                    Ok(mut arp) => {
                                        iprintln!(_stim, "** {:?}", arp);

                                        if !arp.is_a_probe() {
                                            resources
                                                .CACHE
                                                .insert(arp.get_spa(), arp.get_sha())
                                                .ok();
                                        }

                                        // are they asking for us?
                                        if arp.get_oper() == arp::Operation::Request
                                            && arp.get_tpa() == IP
                                        {
                                            // reply the ARP request
                                            let tha = arp.get_sha();
                                            let tpa = arp.get_spa();

                                            arp.set_oper(arp::Operation::Reply);
                                            arp.set_sha(MAC);
                                            arp.set_spa(IP);
                                            arp.set_tha(tha);
                                            arp.set_tpa(tpa);
                                            iprintln!(_stim, "\n** {:?}", arp);

                                            // update the Ethernet header
                                            eth.set_destination(tha);
                                            eth.set_source(MAC);
                                            iprintln!(_stim, "* {:?}", eth);

                                            iprintln!(_stim, "Tx({})", eth.as_bytes().len());
                                            resources.ETH.transmit(eth.as_bytes()).ok().unwrap();
                                        }
                                    }
                                    Err(_arp) => {
                                        iprintln!(_stim, "** {:?}", _arp);
                                    }
                                }
                            } else {
                                iprintln!(_stim, "Err(B)");
                            }
                        }
                        ether::Type::Ipv4 => {
                            if let Ok(mut ip) = ipv4::Packet::parse(eth.payload_mut()) {
                                iprintln!(_stim, "** {:?}", ip);

                                let ip_src = ip.get_source();

                                if !mac_src.is_broadcast() {
                                    resources.CACHE.insert(ip_src, mac_src).ok();
                                }

                                match ip.get_protocol() {
                                    ipv4::Protocol::Icmp => {
                                        if let Ok(icmp) = icmp::Packet::parse(ip.payload_mut()) {
                                            iprintln!(_stim, "*** {:?}", icmp);

                                            if icmp.get_type() == icmp::Type::EchoRequest
                                                && icmp.get_code() == 0
                                            {
                                                let _icmp = icmp
                                                    .set_type(icmp::Type::EchoReply)
                                                    .update_checksum();
                                                iprintln!(_stim, "\n*** {:?}", _icmp);

                                                // update the IP header
                                                let mut ip = ip.set_source(IP);
                                                ip.set_destination(ip_src);
                                                let _ip = ip.update_checksum();
                                                iprintln!(_stim, "** {:?}", _ip);

                                                // update the Ethernet header
                                                eth.set_destination(
                                                    *resources.CACHE.get(&ip_src).unwrap(),
                                                );
                                                eth.set_source(MAC);
                                                iprintln!(_stim, "* {:?}", eth);

                                                iprintln!(_stim, "Tx({})", eth.as_bytes().len());
                                                resources
                                                    .ETH
                                                    .transmit(eth.as_bytes())
                                                    .ok()
                                                    .unwrap();
                                            }
                                        } else {
                                            iprintln!(_stim, "Err(C)");
                                        }
                                    }
                                    ipv4::Protocol::Udp => {
                                        if let Ok(udp) = udp::Packet::parse(ip.payload()) {
                                            iprintln!(_stim, "*** {:?}", udp);

                                            let udp_src = udp.get_source();

                                            if udp.get_destination() == coap::PORT {
                                                if let Ok(coap) =
                                                    coap::Message::parse(udp.payload())
                                                {
                                                    iprintln!(_stim, "**** {:?}", coap);

                                                    let path_is_led = coap
                                                        .options()
                                                        .filter_map(|opt| {
                                                            if opt.number()
                                                                == coap::OptionNumber::UriPath
                                                            {
                                                                Some(opt.value())
                                                            } else {
                                                                None
                                                            }
                                                        })
                                                        .eq([b"led"].iter().cloned());

                                                    let mut resp = coap::Response::BadRequest;

                                                    match coap.get_code().try_into() {
                                                        Ok(coap::Method::Get) => {
                                                            if path_is_led {
                                                                resp = coap::Response::Content;
                                                            }
                                                        }
                                                        Ok(coap::Method::Put) => {
                                                            if path_is_led {
                                                                if let Ok(json) =
                                                                    json::de::from_slice::<Led>(
                                                                        coap.payload(),
                                                                    )
                                                                {
                                                                    iprintln!(
                                                                        _stim,
                                                                        "JSON LED {:?}",
                                                                        json.led
                                                                    );
                                                                    if json.led {
                                                                        resources.LED.set_low();
                                                                    } else {
                                                                        resources.LED.set_high();
                                                                    }
                                                                    resp = coap::Response::Changed;
                                                                }
                                                            }
                                                        }
                                                        _ => {}
                                                    }

                                                    let mut buf = eth.free();

                                                    let mut eth = ether::Frame::new(buf);
                                                    eth.set_destination(
                                                        *resources.CACHE.get(&ip_src).unwrap(),
                                                    );
                                                    eth.set_source(MAC);

                                                    eth.ipv4(|ip| {
                                                        ip.set_source(IP);
                                                        ip.set_destination(ip_src);

                                                        ip.udp(|udp| {
                                                            udp.set_destination(udp_src);
                                                            udp.set_source(coap::PORT);
                                                            udp.coap(0, |coap| {
                                                                coap.set_type(
                                                                    coap::Type::Acknowledgement,
                                                                );
                                                                coap.set_code(resp);

                                                                if resp == coap::Response::Content {
                                                                    coap.set_payload(
                                                                        &json::ser::to_vec::<
                                                                            [u8; 16],
                                                                            _,
                                                                        >(
                                                                            &Led { led: false }
                                                                        )
                                                                        .unwrap(),
                                                                    );
                                                                } else {
                                                                    coap.set_payload(&[]);
                                                                }

                                                                iprintln!(
                                                                    _stim,
                                                                    "\n**** {:?}",
                                                                    coap
                                                                );
                                                            });

                                                            iprintln!(_stim, "*** {:?}", udp);
                                                        });

                                                        iprintln!(_stim, "** {:?}", ip);
                                                    });

                                                    iprintln!(_stim, "* {:?}", eth);

                                                    let bytes = eth.as_bytes();
                                                    iprintln!(_stim, "Tx({})", bytes.len());
                                                    resources.ETH.transmit(bytes).ok().unwrap();
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            } else {
                                iprintln!(_stim, "Err(D)");
                            }
                        }
                        _ => {}
                    }
                } else {
                    iprintln!(_stim, "Err(E)");
                }
            }
            Err(e) => iprintln!(_stim, "Err({:?})", e),
            _ => (),
        }
        resources.EXTI.pr.modify(|_, w| w.pr0().set_bit());
    }
};

struct AsmDelay {}

impl embedded_hal::blocking::delay::DelayMs<u8> for AsmDelay {
    fn delay_ms(&mut self, ms: u8) {
        asm::delay((ms as u32) * (CPU_HZ / 10));
    }
}
