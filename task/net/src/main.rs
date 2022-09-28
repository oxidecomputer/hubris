// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

mod bsp;
mod buf;
mod miim_bridge;
mod server;

pub mod pins;

cfg_if::cfg_if! {
    if #[cfg(feature = "vlan")] {
        mod server_vlan;
        use server_vlan::ServerImpl;
    } else {
        mod server_basic;
        use server_basic::ServerImpl;
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "mgmt")] {
        pub(crate) mod mgmt;
    }
}

mod idl {
    use task_net_api::{
        KszError, KszMacTableEntry, LargePayloadBehavior, MacAddress,
        ManagementCounters, ManagementLinkStatus, MgmtError, PhyError,
        RecvError, SendError, SocketName, UdpMetadata,
    };
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

use core::sync::atomic::{AtomicU32, Ordering};
use zerocopy::AsBytes;

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;
#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::Sys;
use userlib::*;

task_slot!(SYS, sys);

/////////////////////////////////////////////////////////////////////////////
// Configuration things!
//
// Much of this needs to move into the board-level configuration.

/// Claims and calculates the MAC address.  This can only be called once.
fn mac_address() -> &'static [u8; 6] {
    let buf = crate::buf::claim_mac_address();
    let uid = drv_stm32xx_uid::read_uid();
    // Jenkins hash
    let mut hash: u32 = 0;
    for byte in uid.as_bytes() {
        hash = hash.wrapping_add(*byte as u32);
        hash = hash.wrapping_add(hash << 10);
        hash ^= hash >> 6;
    }
    hash = hash.wrapping_add(hash << 3);
    hash ^= hash >> 11;
    hash = hash.wrapping_add(hash >> 15);

    // Locally administered, unicast address
    buf[0] = 0x0e;
    buf[1] = 0x1d;

    // Set the lower 32-bits based on the hashed UID
    buf[2..].copy_from_slice(&hash.to_be_bytes());
    buf
}

const TX_RING_SZ: usize = 4;

const RX_RING_SZ: usize = 4;

/// Notification mask for our IRQ; must match configuration in app.toml.
const ETH_IRQ: u32 = 1 << 0;

/// Notification mask for MDIO timer; must match configuration in app.toml.
const MDIO_TIMER_IRQ: u32 = 1 << 1;

/// Notification mask for optional periodic logging
const WAKE_IRQ: u32 = 1 << 2;

/// Number of entries to maintain in our neighbor cache (ARP/NDP).
const NEIGHBORS: usize = 4;

/////////////////////////////////////////////////////////////////////////////
// Main driver loop.

/// Global count of passes through the driver loop, for inspection through a
/// debugger.
static ITER_COUNT: AtomicU32 = AtomicU32::new(0);

#[export_name = "main"]
fn main() -> ! {
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    // Do any preinit tasks specific to this board.  For hardware which requires
    // explicit clock configuration, this is where the `net` tasks waits for
    // the clock to come up.
    bsp::preinit();

    // Turn on the Ethernet power.
    sys.enable_clock(drv_stm32xx_sys_api::Peripheral::Eth1Rx);
    sys.enable_clock(drv_stm32xx_sys_api::Peripheral::Eth1Tx);
    sys.enable_clock(drv_stm32xx_sys_api::Peripheral::Eth1Mac);

    // Reset the MAC. This is one of two resets that must occur for the MAC to
    // work; the other is below.
    sys.enter_reset(drv_stm32xx_sys_api::Peripheral::Eth1Mac);
    sys.leave_reset(drv_stm32xx_sys_api::Peripheral::Eth1Mac);

    // Reset our MDIO timer.
    sys.enable_clock(drv_stm32xx_sys_api::Peripheral::Tim16);
    sys.enter_reset(drv_stm32xx_sys_api::Peripheral::Tim16);
    sys.leave_reset(drv_stm32xx_sys_api::Peripheral::Tim16);

    // Do preliminary pin configuration
    bsp::configure_ethernet_pins(&sys);

    // Set up our ring buffers.
    let (tx_storage, tx_buffers) = buf::claim_tx_statics();
    let tx_ring = eth::ring::TxRing::new(tx_storage, tx_buffers);
    let (rx_storage, rx_buffers) = buf::claim_rx_statics();
    let rx_ring = eth::ring::RxRing::new(rx_storage, rx_buffers);

    // Create the driver instance.
    let eth = eth::Ethernet::new(
        unsafe { &*device::ETHERNET_MAC::ptr() },
        unsafe { &*device::ETHERNET_MTL::ptr() },
        unsafe { &*device::ETHERNET_DMA::ptr() },
        tx_ring,
        rx_ring,
        unsafe { &*device::TIM16::ptr() },
        MDIO_TIMER_IRQ,
    );

    // Set up the network stack.
    use smoltcp::wire::EthernetAddress;
    let mac = EthernetAddress::from_bytes(mac_address());

    // Configure the server and its local storage arrays (on the stack)
    let ipv6_addr = link_local_iface_addr(mac);

    // Board-dependant initialization (e.g. bringing up the PHYs)
    let bsp = bsp::Bsp::new(&eth, &sys);

    let mut server = ServerImpl::new(&eth, ipv6_addr, mac, bsp);

    // Turn on our IRQ.
    userlib::sys_irq_control(ETH_IRQ, true);

    // Some of the BSPs include a 'wake' function which allows for periodic
    // logging.  We schedule a wake-up before entering the idol_runtime dispatch
    // loop, to make sure that this gets called periodically.
    let mut wake_target_time = sys_get_timer().now;

    // Go!
    loop {
        ITER_COUNT.fetch_add(1, Ordering::Relaxed);

        // Call into smoltcp.
        let poll_result = server.poll(userlib::sys_get_timer().now);
        let any_activity = poll_result.unwrap_or(true);

        if any_activity {
            // Ask the server to iterate over sockets looking for work
            server.wake_sockets();
        } else {
            // No work to do immediately. Wait for an ethernet IRQ or an
            // incoming message, or for a certain amount of time to pass.
            if let Some(wake_interval) = bsp::WAKE_INTERVAL {
                let now = sys_get_timer().now;
                if now >= wake_target_time {
                    server.wake();
                    wake_target_time = now + wake_interval;
                }
                sys_set_timer(Some(wake_target_time), WAKE_IRQ);
            }
            let mut msgbuf = [0u8; ServerImpl::INCOMING_SIZE];
            idol_runtime::dispatch_n(&mut msgbuf, &mut server);
        }
    }
}

/// We can map an Ethernet MAC address into the IPv6 space as follows.
///
/// - The top 64 bits are `fe80::`, putting it in the link-local (non-routable)
///   address space.
/// - The bottom 64 bits are the Interface ID, which we generate with the EUI-64
///   method.
///
/// The EUI-64 transform for a MAC address is given in RFC4291 section 2.5.1,
/// and can be summarized as follows.
///
/// - Insert the bytes `FF FE` in the middle to extend the MAC address to 8
///   bytes.
/// - Flip bit 1 in the first byte, to translate the OUI universal/local bit
///   into the IPv6 universal/local bit.
fn link_local_iface_addr(
    mac: smoltcp::wire::EthernetAddress,
) -> smoltcp::wire::Ipv6Address {
    let mut bytes = [0; 16];
    // Link-local address block.
    bytes[0..2].copy_from_slice(&[0xFE, 0x80]);
    // Bytes 2..8 are all zero.
    // Top three bytes of MAC address...
    bytes[8..11].copy_from_slice(&mac.0[0..3]);
    // ...with administration scope bit flipped.
    bytes[8] ^= 0b0000_0010;
    // Inserted FF FE from EUI64 transform.
    bytes[11..13].copy_from_slice(&[0xFF, 0xFE]);
    // Bottom three bytes of MAC address.
    bytes[13..16].copy_from_slice(&mac.0[3..6]);

    smoltcp::wire::Ipv6Address(bytes)
}

// Place to namespace all the bits generated by our config processor.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/net_config.rs"));
}
