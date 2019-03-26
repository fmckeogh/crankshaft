//! ENC28J60 + smoltcp demo
//!
//! Demonstrates how to use an ENC28J60 with smoltcp by running a simple demo that
//! toggles and returns the current LED state.
//!
//! You can test this program with the following:
//!
//! - `ping 192.168.1.2`. The device will respond to every request (response time should be ~10ms).
//! - `curl 192.168.1.2`. The device will respond with a HTTP response with the current
//! LED state in the body.
//! - Visiting `https://192.168.1.2/`. Every refresh will toggle the LED and the page will
//! reflect the current state.
//!
#![no_std]
#![no_main]

extern crate panic_semihosting;

use core::fmt::Write;
use enc28j60::{smoltcp_phy::Phy, Enc28j60};
use rtfm::app;
use smoltcp::{
    iface::{EthernetInterfaceBuilder, NeighborCache},
    socket::{SocketSet, TcpSocket, TcpSocketBuffer},
    time::Instant,
    wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address},
};
use stm32f1xx_hal::{
    delay::Delay,
    device::{self, SPI1},
    prelude::*,
    serial::Serial,
    spi::Spi,
};

static INDEX_HEADER: &'static [u8] = b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\n\r\n";
static INDEX_BODY: &'static [u8] = include_bytes!("../index.html.br");
static STATUS_HEADER: &'static [u8] =
    b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: http://192.168.1.2\r\n\r\n";

const SRC_MAC: [u8; 6] = [0x20, 0x18, 0x03, 0x01, 0x00, 0x00];
const CHUNK_SIZE: usize = 256;

#[app(device = stm32f1xx_hal::device)]
const APP: () = {
    static mut LED: stm32f1xx_hal::gpio::gpioc::PC13<
        stm32f1xx_hal::gpio::Output<stm32f1xx_hal::gpio::PushPull>,
    > = ();
    static mut SERIAL: stm32f1xx_hal::serial::Tx<device::USART1> = ();
    static mut ETH: enc28j60::smoltcp_phy::Phy<
        'static,
        stm32f1xx_hal::spi::Spi<
            SPI1,
            (
                stm32f1xx_hal::gpio::gpioa::PA5<
                    stm32f1xx_hal::gpio::Alternate<stm32f1xx_hal::gpio::PushPull>,
                >,
                stm32f1xx_hal::gpio::gpioa::PA6<
                    stm32f1xx_hal::gpio::Input<stm32f1xx_hal::gpio::Floating>,
                >,
                stm32f1xx_hal::gpio::gpioa::PA7<
                    stm32f1xx_hal::gpio::Alternate<stm32f1xx_hal::gpio::PushPull>,
                >,
            ),
        >,
        stm32f1xx_hal::gpio::gpioa::PA4<stm32f1xx_hal::gpio::Output<stm32f1xx_hal::gpio::PushPull>>,
        enc28j60::Unconnected,
        stm32f1xx_hal::gpio::gpioa::PA3<stm32f1xx_hal::gpio::Output<stm32f1xx_hal::gpio::PushPull>>,
    > = ();

    static mut RX_BUF: [u8; 1024] = [0u8; 1024];
    static mut TX_BUF: [u8; 1024] = [0u8; 1024];

    #[init(resources = [RX_BUF, TX_BUF])]
    fn init() {
        let core: rtfm::Peripherals = core;
        let device: device::Peripherals = device;

        let mut rcc = device.RCC.constrain();
        let mut afio = device.AFIO.constrain(&mut rcc.apb2);
        let mut flash = device.FLASH.constrain();
        let mut gpioa = device.GPIOA.split(&mut rcc.apb2);
        let mut gpiob = device.GPIOB.split(&mut rcc.apb2);
        let mut gpioc = device.GPIOC.split(&mut rcc.apb2);
        let clocks = rcc.cfgr.freeze(&mut flash.acr);

        // LED
        let mut led = gpioc.pc13.into_push_pull_output(&mut gpioc.crh);
        // turn the LED off during initialization
        led.set_high();

        // Serial
        let mut serial = {
            let tx = gpiob.pb6.into_alternate_push_pull(&mut gpiob.crl);
            let rx = gpiob.pb7;
            let serial = Serial::usart1(
                device.USART1,
                (tx, rx),
                &mut afio.mapr,
                115_200.bps(),
                clocks,
                &mut rcc.apb2,
            );

            serial.split().0
        };
        writeln!(serial, "serial start").unwrap();

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
        writeln!(serial, "spi initialized").unwrap();

        // ENC28J60
        let enc28j60 = {
            let mut ncs = gpioa.pa4.into_push_pull_output(&mut gpioa.crl);
            ncs.set_high();
            let mut reset = gpioa.pa3.into_push_pull_output(&mut gpioa.crl);
            reset.set_high();
            let mut delay = Delay::new(core.SYST, clocks);

            Enc28j60::new(
                spi,
                ncs,
                enc28j60::Unconnected,
                reset,
                &mut delay,
                7168,
                SRC_MAC,
            )
            .ok()
            .unwrap()
        };
        writeln!(serial, "enc26j60 initialized").unwrap();

        // PHY Wrapper
        let mut eth = Phy::new(enc28j60, resources.RX_BUF, resources.TX_BUF);
        writeln!(serial, "eth initialized").unwrap();

        // LED on after initialization
        led.set_low();

        LED = led;
        SERIAL = serial;
        ETH = eth;
    }

    #[idle(resources = [LED, SERIAL, ETH])]
    fn idle() -> ! {
        // Ethernet interface
        let ethernet_addr = EthernetAddress(SRC_MAC);
        let local_addr = Ipv4Address::new(192, 168, 1, 2);
        let ip_addr = IpCidr::new(IpAddress::from(local_addr), 24);
        let mut ip_addrs = [ip_addr];
        let mut neighbor_storage = [None; 16];
        let neighbor_cache = NeighborCache::new(&mut neighbor_storage[..]);
        let mut iface = EthernetInterfaceBuilder::new(resources.ETH)
            .ethernet_addr(ethernet_addr)
            .ip_addrs(&mut ip_addrs[..])
            .neighbor_cache(neighbor_cache)
            .finalize();
        writeln!(resources.SERIAL, "iface initialized").unwrap();

        // Sockets
        let mut server_rx_buffer = [0; 1024];
        let mut server_tx_buffer = [0; 1024];
        let server_socket = TcpSocket::new(
            TcpSocketBuffer::new(&mut server_rx_buffer[..]),
            TcpSocketBuffer::new(&mut server_tx_buffer[..]),
        );

        let mut status_rx_buffer = [0; 1024];
        let mut status_tx_buffer = [0; 1024];
        let status_socket = TcpSocket::new(
            TcpSocketBuffer::new(&mut status_rx_buffer[..]),
            TcpSocketBuffer::new(&mut status_tx_buffer[..]),
        );
        let mut sockets_storage = [None, None];
        let mut sockets = SocketSet::new(&mut sockets_storage[..]);
        let server_handle = sockets.add(server_socket);
        let status_handle = sockets.add(status_socket);
        writeln!(resources.SERIAL, "sockets initialized").unwrap();

        let mut count: u64 = 0;
        let mut cursor: usize = 0;

        loop {
            match iface.poll(&mut sockets, Instant::from_millis(0)) {
                Ok(b) => {
                    if b {
                        {
                            let mut server_socket = sockets.get::<TcpSocket>(server_handle);
                            if !server_socket.is_open() {
                                server_socket.listen(80).unwrap();
                            }
                            if server_socket.can_send() {
                                if cursor == 0 {
                                    writeln!(resources.SERIAL, "tcp:80 sending").unwrap();
                                    writeln!(
                                        resources.SERIAL,
                                        "tcp:80 sent {}",
                                        server_socket.send_slice(INDEX_HEADER).unwrap()
                                    )
                                    .unwrap();
                                }

                                if cursor + CHUNK_SIZE < INDEX_BODY.len() {
                                    writeln!(
                                        resources.SERIAL,
                                        "tcp:80 sent {}",
                                        server_socket
                                            .send_slice(&INDEX_BODY[cursor..(cursor + CHUNK_SIZE)])
                                            .unwrap()
                                    )
                                    .unwrap();
                                    cursor += CHUNK_SIZE;
                                } else if cursor + CHUNK_SIZE > INDEX_BODY.len()
                                    && cursor < INDEX_BODY.len()
                                {
                                    writeln!(
                                        resources.SERIAL,
                                        "tcp:80 sent {}",
                                        server_socket.send_slice(&INDEX_BODY[cursor..]).unwrap()
                                    )
                                    .unwrap();
                                    cursor += CHUNK_SIZE;
                                } else {
                                    cursor = 0;
                                    writeln!(resources.SERIAL, "tcp:80 close").unwrap();
                                    server_socket.close();
                                }
                            }
                        }

                        {
                            let mut status_socket = sockets.get::<TcpSocket>(status_handle);
                            if !status_socket.is_open() {
                                status_socket.listen(81).unwrap();
                            }
                            if status_socket.can_send() {
                                resources.LED.toggle();
                                count += 1;

                                writeln!(resources.SERIAL, "tcp:81 sending").unwrap();
                                status_socket.send_slice(STATUS_HEADER).unwrap();
                                write!(
                                    status_socket,
                                    "{{\r\n\t\"state\": {},\r\n\t\"count\": {}\r\n}}\r\n",
                                    resources.LED.is_set_low(),
                                    count
                                )
                                .unwrap();

                                writeln!(resources.SERIAL, "tcp:81 close").unwrap();
                                status_socket.close();
                            }
                        }
                    }
                }
                Err(e) => {
                    writeln!(resources.SERIAL, "Error: {:?}", e).unwrap();
                }
            }
        }
    }
};
