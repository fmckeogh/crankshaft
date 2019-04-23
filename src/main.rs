#![no_std]
#![no_main]

#[macro_use]
extern crate cortex_m;
extern crate panic_semihosting;

use {
    core::fmt::Write,
    embedded_hal::digital::{OutputPin, StatefulOutputPin},
    enc28j60::{smoltcp_phy::Phy, Enc28j60},
    heapless::{consts::U16, Vec},
    rtfm::app,
    smoltcp::{
        iface::{EthernetInterfaceBuilder, NeighborCache},
        socket::{SocketSet, TcpSocket, TcpSocketBuffer},
        wire::{EthernetAddress, IpAddress, IpCidr, Ipv4Address},
    },
    stm32f4xx_hal::{
        gpio::{
            gpioa::{PA3, PA4, PA5, PA6, PA7},
            gpiod::PD14,
            gpiod::{PD1, PD2, PD3, PD4, PD5, PD6},
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

    static mut MOTOR_DRIVER: MotorDriver<
        PD1<Output<PushPull>>,
        PD2<Output<PushPull>>,
        PD3<Output<PushPull>>,
        PD4<Output<PushPull>>,
        PD5<Output<PushPull>>,
        PD6<Output<PushPull>>,
    > = ();
    static mut MOTOR_CONTROL: ControlState = ControlState::Idle;

    static mut RX_BUF: [u8; 1024] = [0u8; 1024];
    static mut TX_BUF: [u8; 1024] = [0u8; 1024];

    #[init(resources = [RX_BUF, TX_BUF], schedule = [motor])]
    fn init() {
        let mut core: rtfm::Peripherals = core;
        let device: device::Peripherals = device;

        let gpioa = device.GPIOA.split();
        let _gpiob = device.GPIOB.split();
        let _gpioc = device.GPIOC.split();
        let gpiod = device.GPIOD.split();
        let gpioe = device.GPIOE.split();

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
                1_000_000.hz(),
                clocks,
            )
        };
        iprintln!(_stim, "\n\ninit: spi");

        // ENC28J60
        let enc28j60 = {
            let mut rst = gpioa.pa3.into_push_pull_output();
            rst.set_high();
            let mut ncs = gpioa.pa4.into_push_pull_output();
            ncs.set_high();
            let mut delay = NopDelay {};

            Enc28j60::new(
                spi,
                ncs,
                enc28j60::Unconnected,
                rst,
                &mut delay,
                7168,
                SRC_MAC,
            )
            .unwrap()
        };
        iprintln!(_stim, "init: enc26j60");

        // PHY Wrapper
        let eth = Phy::new(enc28j60, resources.RX_BUF, resources.TX_BUF);
        iprintln!(_stim, "init: phy");

        // Motor setup
        let motor_driver = {
            let a = Phase::new(
                gpiod.pd1.into_push_pull_output(),
                gpiod.pd2.into_push_pull_output(),
            );
            let b = Phase::new(
                gpiod.pd3.into_push_pull_output(),
                gpiod.pd4.into_push_pull_output(),
            );
            let c = Phase::new(
                gpiod.pd5.into_push_pull_output(),
                gpiod.pd6.into_push_pull_output(),
            );
            MotorDriver::new(a, b, c)
        };
        schedule
            .motor(rtfm::Instant::now() + CPU_HZ.cycles())
            .unwrap();

        iprintln!(_stim, "init: complete\n");
        LED = led;
        ITM = core.ITM;
        ETH = eth;
        MOTOR_DRIVER = motor_driver;
    }

    #[idle(resources = [LED, ITM, ETH, MOTOR_CONTROL])]
    fn idle() -> ! {
        resources.ITM.lock(|itm| {
            iprintln!(&mut itm.stim[0], "motor task");
        });

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
        resources.ITM.lock(|itm| {
            iprintln!(&mut itm.stim[0], "idle: iface");
        });

        // Sockets
        let mut server_rx_buffer = [0; 1024];
        let mut server_tx_buffer = [0; 1024];
        let server_socket = TcpSocket::new(
            TcpSocketBuffer::new(&mut server_rx_buffer[..]),
            TcpSocketBuffer::new(&mut server_tx_buffer[..]),
        );

        let mut sockets_storage = [None, None];
        let mut sockets = SocketSet::new(&mut sockets_storage[..]);
        let server_handle = sockets.add(server_socket);
        resources.ITM.lock(|itm| {
            iprintln!(&mut itm.stim[0], "idle: sockets");
        });

        let mut count: u64 = 0;
        let mut cursor: usize = 0;

        loop {
            match iface.poll(&mut sockets, smoltcp::time::Instant::from_millis(0)) {
                Ok(b) => {
                    if b {
                        {
                            let mut server_socket = sockets.get::<TcpSocket>(server_handle);
                            if !server_socket.is_open() {
                                server_socket
                                    .listen(80)
                                    .expect("Failed to listen on port 80");
                                resources.ITM.lock(|itm| {
                                    iprintln!(&mut itm.stim[0], "tcp:80 listening");
                                });
                            }
                            if cursor == 0 && server_socket.can_recv() {
                                let mut buf = [0u8; 1024];
                                let len = server_socket
                                    .recv_slice(&mut buf)
                                    .expect("Failed to receive slice");

                                let request = Request::from_str(
                                    core::str::from_utf8(&buf[..len])
                                        .expect("Request not valid UTF8"),
                                )
                                .expect("Failed to parse HTTP request");

                                resources.ITM.lock(|itm| {
                                    iprintln!(&mut itm.stim[0], "tcp:80 receiving {:?}", request);
                                });

                                if request.method == "GET" && request.route == "/" {
                                    if server_socket.can_send() {
                                        if cursor == 0 {
                                            resources.ITM.lock(|itm| {
                                                iprintln!(&mut itm.stim[0], "tcp:80 sending");
                                            });
                                            let len =
                                                server_socket.send_slice(INDEX_HEADER).unwrap();
                                            resources.ITM.lock(|itm| {
                                                iprintln!(&mut itm.stim[0], "tcp:80 sent {}", len);
                                            });
                                        }

                                        let len = server_socket
                                            .send_slice(&INDEX_BODY[cursor..(cursor + CHUNK_SIZE)])
                                            .unwrap();
                                        resources.ITM.lock(|itm| {
                                            iprintln!(&mut itm.stim[0], "tcp:80 sent {}", len);
                                        });
                                        cursor += CHUNK_SIZE;
                                    }
                                } else if request.method == "POST" {
                                    resources.MOTOR_CONTROL.lock(|c| {
                                        *c = match request.route {
                                            "/f" => ControlState::Forward,
                                            "/r" => ControlState::Reverse,
                                            "/s" => ControlState::Idle,
                                            _ => ControlState::Idle,
                                        }
                                    });

                                    resources.ITM.lock(|itm| {
                                        iprintln!(&mut itm.stim[0], "tcp:80 sending");
                                    });
                                    server_socket.send_slice(STATUS_HEADER).unwrap();
                                    write!(
                                        server_socket,
                                        "{{\r\n\t\"state\": \"{}\",\r\n\t\"count\": {}\r\n}}\r\n",
                                        false, false,
                                    )
                                    .unwrap();

                                    resources.ITM.lock(|itm| {
                                        iprintln!(&mut itm.stim[0], "tcp:80 close");
                                    });
                                    server_socket.close();
                                }
                            }

                            if server_socket.can_send() {
                                if cursor + CHUNK_SIZE < INDEX_BODY.len() {
                                    let len = server_socket
                                        .send_slice(&INDEX_BODY[cursor..(cursor + CHUNK_SIZE)])
                                        .unwrap();
                                    resources.ITM.lock(|itm| {
                                        iprintln!(&mut itm.stim[0], "tcp:80 sent {}", len);
                                    });
                                    cursor += CHUNK_SIZE;
                                } else if cursor + CHUNK_SIZE > INDEX_BODY.len()
                                    && cursor < INDEX_BODY.len()
                                {
                                    let len =
                                        server_socket.send_slice(&INDEX_BODY[cursor..]).unwrap();
                                    resources.ITM.lock(|itm| {
                                        iprintln!(&mut itm.stim[0], "tcp:80 sent {}", len);
                                    });
                                    cursor += CHUNK_SIZE;
                                } else {
                                    cursor = 0;
                                    resources.ITM.lock(|itm| {
                                        iprintln!(&mut itm.stim[0], "tcp:80 close");
                                    });
                                    server_socket.close();
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    resources.ITM.lock(|itm| {
                        iprintln!(&mut itm.stim[0], "Error: {:?}", e);
                    });
                }
            }
        }
    }

    #[task(priority = 2, schedule = [motor], resources = [ITM, MOTOR_DRIVER, MOTOR_CONTROL])]
    fn motor() {
        let _stim = &mut resources.ITM.stim[0];

        match *resources.MOTOR_CONTROL {
            ControlState::Idle => {
                resources.MOTOR_DRIVER.set_idle();
            }
            ControlState::Forward => {
                resources.MOTOR_DRIVER.step(false);
            }
            ControlState::Reverse => {
                resources.MOTOR_DRIVER.step(true);
            }
            _ => unimplemented!(),
        }

        iprintln!(
            _stim,
            "motor task: {:?}, {:?}",
            resources.MOTOR_CONTROL,
            resources.MOTOR_DRIVER.comm_state
        );

        schedule.motor(scheduled + (CPU_HZ / 32).cycles()).unwrap();
    }

    extern "C" {
        fn FLASH();
    }
};

struct NopDelay;

impl embedded_hal::blocking::delay::DelayMs<u8> for NopDelay {
    fn delay_ms(&mut self, ms: u8) {
        cortex_m::asm::delay(u32::from(ms) * CPU_HZ / 1000);
    }
}

pub struct MotorDriver<
    AL: OutputPin + StatefulOutputPin,
    AH: OutputPin + StatefulOutputPin,
    BL: OutputPin + StatefulOutputPin,
    BH: OutputPin + StatefulOutputPin,
    CL: OutputPin + StatefulOutputPin,
    CH: OutputPin + StatefulOutputPin,
> {
    pub a: Phase<AL, AH>,
    pub b: Phase<BL, BH>,
    pub c: Phase<CL, CH>,
    pub comm_state: CommutationState,
}

#[derive(Debug)]
pub enum ControlState {
    Idle,
    Brake,
    Forward,
    Reverse,
}

#[derive(Debug)]
pub enum CommutationState {
    AB,
    AC,
    BC,
    BA,
    CA,
    CB,
}

impl CommutationState {
    fn next(&self) -> Self {
        match self {
            CommutationState::AB => CommutationState::AC,
            CommutationState::AC => CommutationState::BC,
            CommutationState::BC => CommutationState::BA,
            CommutationState::BA => CommutationState::CA,
            CommutationState::CA => CommutationState::CB,
            CommutationState::CB => CommutationState::AB,
        }
    }

    fn previous(&self) -> Self {
        match self {
            CommutationState::AB => CommutationState::CB,
            CommutationState::AC => CommutationState::AB,
            CommutationState::BC => CommutationState::AC,
            CommutationState::BA => CommutationState::BC,
            CommutationState::CA => CommutationState::BA,
            CommutationState::CB => CommutationState::AB,
        }
    }
}

impl<
        A_L: OutputPin + StatefulOutputPin,
        A_H: OutputPin + StatefulOutputPin,
        B_L: OutputPin + StatefulOutputPin,
        B_H: OutputPin + StatefulOutputPin,
        C_L: OutputPin + StatefulOutputPin,
        C_H: OutputPin + StatefulOutputPin,
    > MotorDriver<A_L, A_H, B_L, B_H, C_L, C_H>
{
    fn new(a: Phase<A_L, A_H>, b: Phase<B_L, B_H>, c: Phase<C_L, C_H>) -> Self {
        Self {
            a,
            b,
            c,
            comm_state: CommutationState::AB,
        }
    }

    fn step(&mut self, direction: bool) {
        self.comm_state = match direction {
            true => self.comm_state.next(),
            false => self.comm_state.previous(),
        };
        //self.comm_state = self.comm_state.next();

        match self.comm_state {
            CommutationState::AB => {
                self.a.set_high();
                self.b.set_low();
                self.c.set_floating();
            }
            CommutationState::AC => {
                self.a.set_high();
                self.b.set_floating();
                self.c.set_low();
            }
            CommutationState::BC => {
                self.a.set_floating();
                self.b.set_high();
                self.c.set_low();
            }
            CommutationState::BA => {
                self.a.set_low();
                self.b.set_high();
                self.c.set_floating();
            }
            CommutationState::CA => {
                self.a.set_low();
                self.b.set_floating();
                self.c.set_high();
            }
            CommutationState::CB => {
                self.a.set_floating();
                self.b.set_low();
                self.c.set_high();
            }
        }
    }

    fn set_idle(&mut self) {
        self.a.set_floating();
        self.b.set_floating();
        self.c.set_floating();
    }
}

pub struct Phase<L: OutputPin + StatefulOutputPin, H: OutputPin + StatefulOutputPin> {
    low_gate: L,
    high_gate: H,
}

impl<L: OutputPin + StatefulOutputPin, H: OutputPin + StatefulOutputPin> Phase<L, H> {
    fn new(mut low_gate: L, mut high_gate: H) -> Self {
        high_gate.set_low();
        low_gate.set_low();

        Self {
            low_gate,
            high_gate,
        }
    }

    fn set_floating(&mut self) {
        self.high_gate.set_low();
        self.low_gate.set_low();
    }

    /// Set the phase to VIN
    fn set_high(&mut self) {
        self.low_gate.set_low();

        self.high_gate.set_high();
    }

    /// Set the phase to ground
    fn set_low(&mut self) {
        self.high_gate.set_low();
        self.low_gate.set_high();
    }

    fn is_set_high(&mut self) -> bool {
        self.high_gate.is_set_high() && self.low_gate.is_set_low()
    }

    fn is_set_low(&mut self) -> bool {
        self.low_gate.is_set_high() && self.high_gate.is_set_low()
    }
}

#[derive(Debug)]
struct Request<'a> {
    method: &'a str,
    route: &'a str,
    headers: Vec<(&'a str, &'a str), U16>,
    body: Option<&'a str>,
}

impl<'a> Request<'a> {
    fn from_str(input: &'a str) -> Result<Self, ()> {
        let mut request = input.split("\n\n");
        let mut head = request.next().expect("Couldn't get HTTP headers").lines();
        let mut metadata = head.next().expect("Couldn't get first line").split(" ");
        let method = metadata.next().expect("Couldn't get method");
        let route = metadata.next().expect("Couldn't get route");
        let headers = head
            .filter(|s| s.contains(":"))
            .map(|raw| {
                let mut iter = raw.split(": ");
                let key = iter.next().expect("Couldn't get key");
                let value = iter.next().expect("Couldn't get value");

                (key, value)
            })
            .collect();

        let body = request.next();

        Ok(Self {
            method,
            route,
            headers,
            body,
        })
    }
}
