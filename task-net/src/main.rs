#![no_std]
#![no_main]

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use userlib::{Generation, Task, TaskId};

use drv_stm32h7_eth as eth;
use drv_stm32h7_rcc_api::Rcc;
use stm32h7::stm32h743 as device;

#[cfg(not(feature = "standalone"))]
const RCC: Task = Task::rcc_driver;
#[cfg(feature = "standalone")]
const RCC: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const GPIO: Task = Task::gpio_driver;
#[cfg(feature = "standalone")]
const GPIO: Task = Task::anonymous;

const TX_RING_SZ: usize = 4;

/// Address used on the MDIO link by our Ethernet PHY. Different vendors have
/// different defaults for this, it will likely need to become configurable.
const PHYADDR: u8 = 0x01;

fn claim_tx_statics() -> (
    &'static mut [eth::ring::TxDesc; TX_RING_SZ],
    &'static mut [eth::ring::Buffer; TX_RING_SZ],
) {
    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::SeqCst) {
        panic!()
    }

    let descs = {
        #[link_section = ".eth_bulk"]
        static mut TX_DESC: MaybeUninit<[eth::ring::TxDesc; TX_RING_SZ]> =
            MaybeUninit::uninit();
        // Safety: unsafe because referencing a static mut; we have ensured that
        // this only happens once by the structure of this function.
        let descs = unsafe { &mut TX_DESC };
        // Safety: unsafe because we're casting and then dereferencing a raw
        // pointer; we're transmuting from MaybeUninit<[]> to [MaybeUninit<>]
        // which is ok.
        let descs: &'static mut [MaybeUninit<eth::ring::TxDesc>; TX_RING_SZ] =
            unsafe { &mut *(descs as *mut _ as *mut _) };
        for uninit_desc in descs.iter_mut() {
            *uninit_desc = MaybeUninit::new(eth::ring::TxDesc::new());
        }
        // Safety: unsafe because we're casting away the MaybeUninit and
        // dereferencing the result. We've fully initialized it just now, so
        // we're good.
        unsafe { &mut *(descs as *mut _ as *mut _) }
    };
    let bufs = {
        #[link_section = ".eth_bulk"]
        static mut TX_BUF: MaybeUninit<[eth::ring::Buffer; TX_RING_SZ]> =
            MaybeUninit::uninit();
        // Safety: unsafe because referencing a static mut; we have ensured that
        // this only happens once by the structure of this function.
        let bufs = unsafe { &mut TX_BUF };
        // Safety: unsafe because we're casting and then dereferencing a raw
        // pointer; we're transmuting from MaybeUninit<[]> to [MaybeUninit<>]
        // which is ok.
        let bufs: &'static mut [MaybeUninit<eth::ring::Buffer>; TX_RING_SZ] =
            unsafe { &mut *(bufs as *mut _ as *mut _) };
        for uninit_buf in bufs.iter_mut() {
            *uninit_buf = MaybeUninit::new(eth::ring::Buffer::new());
        }
        // Safety: unsafe because we're casting away the MaybeUninit and
        // dereferencing the result. We've fully initialized it just now, so
        // we're good.
        unsafe { &mut *(bufs as *mut _ as *mut _) }
    };

    (descs, bufs)
}

const RX_RING_SZ: usize = 4;

fn claim_rx_statics() -> (
    &'static mut [eth::ring::RxDesc; RX_RING_SZ],
    &'static mut [eth::ring::Buffer; RX_RING_SZ],
) {
    static TAKEN: AtomicBool = AtomicBool::new(false);
    if TAKEN.swap(true, Ordering::SeqCst) {
        panic!()
    }

    let descs = {
        #[link_section = ".eth_bulk"]
        static mut RX_DESC: MaybeUninit<[eth::ring::RxDesc; RX_RING_SZ]> =
            MaybeUninit::uninit();
        // Safety: unsafe because referencing a static mut; we have ensured that
        // this only happens once by the structure of this function.
        let descs = unsafe { &mut RX_DESC };
        // Safety: unsafe because we're casting and then dereferencing a raw
        // pointer; we're transmuting from MaybeUninit<[]> to [MaybeUninit<>]
        // which is ok.
        let descs: &'static mut [MaybeUninit<eth::ring::RxDesc>; RX_RING_SZ] =
            unsafe { &mut *(descs as *mut _ as *mut _) };
        for uninit_desc in descs.iter_mut() {
            *uninit_desc = MaybeUninit::new(eth::ring::RxDesc::new());
        }
        // Safety: unsafe because we're casting away the MaybeUninit and
        // dereferencing the result. We've fully initialized it just now, so
        // we're good.
        unsafe { &mut *(descs as *mut _ as *mut _) }
    };
    let bufs = {
        #[link_section = ".eth_bulk"]
        static mut RX_BUF: MaybeUninit<[eth::ring::Buffer; RX_RING_SZ]> =
            MaybeUninit::uninit();
        // Safety: unsafe because referencing a static mut; we have ensured that
        // this only happens once by the structure of this function.
        let bufs = unsafe { &mut RX_BUF };
        // Safety: unsafe because we're casting and then dereferencing a raw
        // pointer; we're transmuting from MaybeUninit<[]> to [MaybeUninit<>]
        // which is ok.
        let bufs: &'static mut [MaybeUninit<eth::ring::Buffer>; RX_RING_SZ] =
            unsafe { &mut *(bufs as *mut _ as *mut _) };
        for uninit_buf in bufs.iter_mut() {
            *uninit_buf = MaybeUninit::new(eth::ring::Buffer::new());
        }
        // Safety: unsafe because we're casting away the MaybeUninit and
        // dereferencing the result. We've fully initialized it just now, so
        // we're good.
        unsafe { &mut *(bufs as *mut _ as *mut _) }
    };

    (descs, bufs)
}

#[export_name = "main"]
fn main() -> ! {
    let rcc = TaskId::for_index_and_gen(RCC as usize, Generation::default());
    let rcc = Rcc::from(rcc);
    rcc.enable_clock(drv_stm32h7_rcc_api::Peripheral::Eth1Rx);
    rcc.enable_clock(drv_stm32h7_rcc_api::Peripheral::Eth1Tx);
    rcc.enable_clock(drv_stm32h7_rcc_api::Peripheral::Eth1Mac);

    rcc.enter_reset(drv_stm32h7_rcc_api::Peripheral::Eth1Mac);
    rcc.leave_reset(drv_stm32h7_rcc_api::Peripheral::Eth1Mac);

    configure_ethernet_pins();

    let (tx_storage, tx_buffers) = claim_tx_statics();
    let tx_ring = eth::ring::TxRing::new(tx_storage, tx_buffers);
    let (rx_storage, rx_buffers) = claim_rx_statics();
    let rx_ring = eth::ring::RxRing::new(rx_storage, rx_buffers);

    let eth = eth::Ethernet::new(
        unsafe { &*device::ETHERNET_MAC::ptr() },
        unsafe { &*device::ETHERNET_MTL::ptr() },
        unsafe { &*device::ETHERNET_DMA::ptr() },
        tx_ring,
        rx_ring,
    );

    use smoltcp::iface::Neighbor;
    use smoltcp::socket::SocketSet;
    use smoltcp::wire::{IpAddress, Ipv6Address};

    static FAKE_MAC: &[u8] = &[0x02, 0x04, 0x06, 0x08, 0x0A, 0x0C];

    let ipv6_addr =
        Ipv6Address::new(0xfe80, 0, 0, 0, 0x0004, 0x06ff, 0xfe08, 0x0a0c);

    let mac = smoltcp::wire::EthernetAddress::from_bytes(FAKE_MAC);
    let ipv6_net = smoltcp::wire::IpCidr::from(smoltcp::wire::Ipv6Cidr::new(
        ipv6_addr, 64,
    ));

    let ipv4_addr = smoltcp::wire::Ipv4Address::from_bytes(&[169, 254, 31, 12]);
    let ipv4_net = smoltcp::wire::IpCidr::from(smoltcp::wire::Ipv4Cidr::new(
        ipv4_addr, 16,
    ));

    let mut ip_addrs = [ipv6_net, ipv4_net];
    let mut neighbor_cache_storage: [Option<(IpAddress, Neighbor)>; 16] =
        [None; 16];
    let neighbor_cache =
        smoltcp::iface::NeighborCache::new(&mut neighbor_cache_storage[..]);

    let mut eth = smoltcp::iface::EthernetInterfaceBuilder::new(eth)
        .ethernet_addr(mac)
        .ip_addrs(&mut ip_addrs[..])
        .neighbor_cache(neighbor_cache)
        .finalize();
    let mut socket_set = [None, None];
    let mut socket_set = SocketSet::new(&mut socket_set[..]);

    let mii_basic_control = eth
        .device_mut()
        .smi_read(PHYADDR, eth::SmiClause22Register::Control);
    let mii_basic_control = mii_basic_control
        | 1 << 12 // AN enable
        | 1 << 9 // restart autoneg
        ;
    eth.device_mut().smi_write(
        PHYADDR,
        eth::SmiClause22Register::Control,
        mii_basic_control,
    );

    // Wait for link-up
    while eth
        .device_mut()
        .smi_read(PHYADDR, eth::SmiClause22Register::Status)
        & (1 << 2)
        == 0
    {
        userlib::hl::sleep_for(1);
    }

    const ETH_IRQ: u32 = 1;
    userlib::sys_irq_control(ETH_IRQ, true);

    loop {
        let poll_result = eth.poll(
            &mut socket_set,
            smoltcp::time::Instant::from_millis(
                userlib::sys_get_timer().now as i64,
            ),
        );

        let no_block = match poll_result {
            Err(_) => {
                // TODO counters would be good
                true
            }
            Ok(activity) => activity,
        };

        if !no_block {
            userlib::hl::recv(
                &mut [],
                ETH_IRQ,
                &mut eth,
                |eth, notification| {
                    if notification & ETH_IRQ != 0 {
                        eth.device_mut().on_interrupt();
                        userlib::sys_irq_control(ETH_IRQ, true);
                    }
                },
                |_, _: u32, _| {
                    // We weren't expecting messages.
                    Ok::<_, u32>(())
                },
            );
        }
    }
}

fn configure_ethernet_pins() {
    // TODO this mapping is hard-coded for the STM32H7 Nucleo board!
    //
    // This board's mapping:
    //
    // RMII REF CLK     PA1
    // MDIO             PA2
    // RMII RX DV       PA7
    //
    // MDC              PC1
    // RMII RXD0        PC4
    // RMII RXD1        PC5
    //
    // RMII TX EN       PG11
    // RMII TXD1        PB13 <-- port B
    // RMII TXD0        PG13
    use drv_stm32h7_gpio_api::*;

    let gpio = Gpio::from(TaskId::for_index_and_gen(
        GPIO as usize,
        Generation::default(),
    ));
    let eth_af = Alternate::AF11;

    gpio.configure(
        Port::A,
        (1 << 1) | (1 << 2) | (1 << 7),
        Mode::Alternate,
        OutputType::PushPull,
        Speed::VeryHigh,
        Pull::None,
        eth_af,
    )
    .unwrap();
    gpio.configure(
        Port::B,
        1 << 13,
        Mode::Alternate,
        OutputType::PushPull,
        Speed::VeryHigh,
        Pull::None,
        eth_af,
    )
    .unwrap();
    gpio.configure(
        Port::C,
        (1 << 1) | (1 << 4) | (1 << 5),
        Mode::Alternate,
        OutputType::PushPull,
        Speed::VeryHigh,
        Pull::None,
        eth_af,
    )
    .unwrap();
    gpio.configure(
        Port::G,
        (1 << 11) | (1 << 12) | (1 << 13),
        Mode::Alternate,
        OutputType::PushPull,
        Speed::VeryHigh,
        Pull::None,
        eth_af,
    )
    .unwrap();
}
