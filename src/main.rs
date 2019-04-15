#![no_std]
#![no_main]

#[macro_use]
extern crate cortex_m;
extern crate panic_itm;

use {
    core::fmt::Write,
    embedded_hal::digital::StatefulOutputPin,
    enc28j60::{smoltcp_phy::Phy, Enc28j60},
    rtfm::app,
    smoltcp::{
        iface::{EthernetInterfaceBuilder, NeighborCache},
        socket::{SocketSet, TcpSocket, TcpSocketBuffer},
        time::Instant,
        wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address},
    },
    stm32f4xx_hal::{
        delay::Delay,
        gpio::{
            gpioa::{PA3, PA4, PA5, PA6, PA7},
            gpiod::PD14,
            Alternate, Output, PushPull, AF5,
        },
        prelude::*,
        spi::Spi,
        stm32::{self as device, SPI1},
    },
};

const CPU_HZ: u32 = 50_000_000;

static INDEX_HEADER: &'static [u8] = b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\n\r\n";
static INDEX_BODY: &'static [u8] = include_bytes!("../index.html.br");
static STATUS_HEADER: &'static [u8] =
    b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: http://192.168.1.2\r\n\r\n";

const SRC_MAC: [u8; 6] = [0x20, 0x18, 0x03, 0x01, 0x00, 0x00];
const CHUNK_SIZE: usize = 256;

#[app(device = stm32f4xx_hal::stm32)]
const APP: () = {
    static mut LED: PD14<Output<PushPull>> = ();
    static mut ITM: cortex_m::peripheral::ITM = ();
    static mut ETH: Phy<
        'static,
        Spi<
            SPI1,
            (
                PA5<Alternate<AF5>>,
                PA6<Alternate<AF5>>,
                PA7<Alternate<AF5>>,
            ),
        >,
        PA4<Output<PushPull>>,
        enc28j60::Unconnected,
        PA3<Output<PushPull>>,
    > = ();
    static mut RX_BUF: [u8; 1024] = [0u8; 1024];
    static mut TX_BUF: [u8; 1024] = [0u8; 1024];

    #[init(resources = [RX_BUF, TX_BUF])]
    fn init() {
        let mut core: rtfm::Peripherals = core;
        let device: device::Peripherals = device;

        let gpioa = device.GPIOA.split();
        let _gpiob = device.GPIOB.split();
        let _gpioc = device.GPIOC.split();
        let gpiod = device.GPIOD.split();

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

        let _stim = &mut core.ITM.stim[0];

        // LED
        let mut led = gpiod.pd14.into_push_pull_output();
        // turn the LED off during initialization
        led.set_high();

        // SPI
        let spi = {
            let sck = gpioa.pa5.into_alternate_af5();
            let miso = gpioa.pa6.into_alternate_af5();
            let mosi = gpioa.pa7.into_alternate_af5();

            Spi::spi1(
                device.SPI1,
                (sck, miso, mosi),
                enc28j60::MODE,
                16_000_000.hz(),
                clocks,
            )
        };
        iprintln!(_stim, "spi initialized");

        // ENC28J60
        let enc28j60 = {
            let mut rst = gpioa.pa3.into_push_pull_output();
            rst.set_high();
            let mut ncs = gpioa.pa4.into_push_pull_output();
            ncs.set_high();
            let mut delay = Delay::new(core.SYST, clocks);

            Enc28j60::new(
                spi,
                ncs,
                enc28j60::Unconnected,
                rst,
                &mut delay,
                7168,
                SRC_MAC,
            )
            .ok()
            .unwrap()
        };
        iprintln!(_stim, "enc26j60 initialized");

        // PHY Wrapper
        let eth = Phy::new(enc28j60, resources.RX_BUF, resources.TX_BUF);
        iprintln!(_stim, "phy initialized");

        iprintln!(_stim, "init complete");
        LED = led;
        ITM = core.ITM;
        ETH = eth;
    }

    #[idle(resources = [LED, ITM, ETH])]
    fn idle() -> ! {
        let _stim = &mut resources.ITM.stim[0];
        iprintln!(_stim, "start idle");

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
        iprintln!(_stim, "iface initialized");

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
        iprintln!(_stim, "sockets initialized");

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
                                    iprintln!(_stim, "tcp:80 sending");
                                    iprintln!(
                                        _stim,
                                        "tcp:80 sent {}",
                                        server_socket.send_slice(INDEX_HEADER).unwrap()
                                    );
                                }

                                if cursor + CHUNK_SIZE < INDEX_BODY.len() {
                                    iprintln!(
                                        _stim,
                                        "tcp:80 sent {}",
                                        server_socket
                                            .send_slice(&INDEX_BODY[cursor..(cursor + CHUNK_SIZE)])
                                            .unwrap()
                                    );
                                    cursor += CHUNK_SIZE;
                                } else if cursor + CHUNK_SIZE > INDEX_BODY.len()
                                    && cursor < INDEX_BODY.len()
                                {
                                    iprintln!(
                                        _stim,
                                        "tcp:80 sent {}",
                                        server_socket.send_slice(&INDEX_BODY[cursor..]).unwrap()
                                    );
                                    cursor += CHUNK_SIZE;
                                } else {
                                    cursor = 0;
                                    iprintln!(_stim, "tcp:80 close");
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

                                iprintln!(_stim, "tcp:81 sending");
                                status_socket.send_slice(STATUS_HEADER).unwrap();
                                write!(
                                    status_socket,
                                    "{{\r\n\t\"state\": {},\r\n\t\"count\": {}\r\n}}\r\n",
                                    resources.LED.is_set_low(),
                                    count
                                )
                                .unwrap();

                                iprintln!(_stim, "tcp:81 close");
                                status_socket.close();
                            }
                        }
                    }
                }
                Err(e) => {
                    iprintln!(_stim, "Error: {:?}", e);
                }
            }
        }
    }
};
