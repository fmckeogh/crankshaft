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
use embedded_hal::digital::StatefulOutputPin;
use enc28j60::Enc28j60;
use hal::delay::Delay;
use hal::prelude::*;
use hal::spi::Spi;
use hal::stm32 as pac;
use heapless::consts::*;
use heapless::FnvIndexMap;
use jnet::{arp, coap, ether, icmp, ipv4, mac, udp, Buffer};
use rt::{entry, exception, ExceptionFrame};

/* Constants */
const KB: u16 = 1024;

/* Network configuration */
const MAC: mac::Addr = mac::Addr([0x20, 0x18, 0x03, 0x01, 0x00, 0x00]);
const IP: ipv4::Addr = ipv4::Addr([192, 168, 1, 33]);

// LED resource
#[derive(Deserialize, Serialize)]
struct Led {
    led: bool,
}

const CPU_HZ: u32 = 50_000_000;

#[entry]
fn main() -> ! {
    let mut cp = cortex_m::Peripherals::take().unwrap();
    let dp = pac::Peripherals::take().unwrap();

    let mut gpioa = dp.GPIOA.split();

    //let clocks = dp.RCC.constrain().cfgr.freeze();

    let clocks = {
        // Power mode
        dp.PWR.cr.modify(|_, w| unsafe { w.vos().bits(0x11) });
        // Flash latency
        dp.FLASH
            .acr
            .modify(|_, w| unsafe { w.latency().bits(0x11) });

        let rcc = dp.RCC.constrain();
        rcc.cfgr
            .sysclk(CPU_HZ.hz())
            .pclk1((CPU_HZ / 2).hz())
            .pclk2(CPU_HZ.hz())
            .hclk(CPU_HZ.hz())
            .freeze()
    };

    let _stim = &mut cp.ITM.stim[0];

    // LED
    let mut gpioc = dp.GPIOC.split();
    let mut led = gpioc.pc13.into_push_pull_output();
    // turn the LED off during initialization
    led.set_high();

    // SPI
    let mut rst = gpioa.pa3.into_push_pull_output();
    rst.set_high();
    let mut ncs = gpioa.pa4.into_push_pull_output();
    ncs.set_high();
    let spi = {
        let sck = gpioa.pa5.into_alternate_af5();
        let miso = gpioa.pa6.into_alternate_af5();
        let mosi = gpioa.pa7.into_alternate_af5();

        Spi::spi1(
            dp.SPI1,
            (sck, miso, mosi),
            enc28j60::MODE,
            16_000_000.hz(),
            clocks,
        )
    };

    // ENC28J60
    let mut delay = Delay::new(cp.SYST, clocks);
    let mut enc28j60 = Enc28j60::new(
        spi,
        ncs,
        enc28j60::Unconnected,
        rst,
        &mut delay,
        7 * KB,
        MAC.0,
    )
    .ok()
    .unwrap();

    // LED on after initialization
    led.set_low();

    // FIXME some frames are lost when sending right after initialization
    delay.delay_ms(100_u8);

    let mut cache = FnvIndexMap::<_, _, U8>::new();

    let mut buf = [0u8; 128];
    iprintln!(_stim, "init complete");
    loop {
        match enc28j60.next_packet() {
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
                                            cache.insert(arp.get_spa(), arp.get_sha()).ok();
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
                                            enc28j60.transmit(eth.as_bytes()).ok().unwrap();
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
                                    cache.insert(ip_src, mac_src).ok();
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
                                                eth.set_destination(*cache.get(&ip_src).unwrap());
                                                eth.set_source(MAC);
                                                iprintln!(_stim, "* {:?}", eth);

                                                iprintln!(_stim, "Tx({})", eth.as_bytes().len());
                                                enc28j60.transmit(eth.as_bytes()).ok().unwrap();
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
                                                                    if json.led {
                                                                        led.set_low();
                                                                    } else {
                                                                        led.set_high();
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
                                                        *cache.get(&ip_src).unwrap(),
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
                                                                            &Led {
                                                                                led: led
                                                                                    .is_set_low(),
                                                                            },
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
                                                    enc28j60.transmit(bytes).ok().unwrap();
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
            Err(e) => iprintln!(_stim, "Err(E)"),
            _ => (),
        }
    }
}

#[exception]
fn HardFault(ef: &ExceptionFrame) -> ! {
    panic!("{:#?}", ef);
}

#[exception]
fn DefaultHandler(irqn: i16) {
    panic!("Unhandled exception (IRQn = {})", irqn);
}
