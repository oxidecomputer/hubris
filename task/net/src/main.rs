// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

pub mod pins;

mod bsp_support;
mod buf;
mod miim_bridge;
mod server;

// Select the BSP based on the target board
#[cfg_attr(
    any(target_board = "nucleo-h743zi2", target_board = "nucleo-h753zi"),
    path = "bsp/nucleo_h7.rs"
)]
#[cfg_attr(
    any(
        target_board = "sidecar-b",
        target_board = "sidecar-c",
        target_board = "sidecar-d",
    ),
    path = "bsp/sidecar_bcd.rs"
)]
#[cfg_attr(
    any(
        target_board = "gimlet-b",
        target_board = "gimlet-c",
        target_board = "gimlet-d",
        target_board = "gimlet-e",
        target_board = "gimlet-f",
    ),
    path = "bsp/gimlet_bcdef.rs"
)]
#[cfg_attr(
    any(target_board = "psc-b", target_board = "psc-c"),
    path = "bsp/psc_bc.rs"
)]
#[cfg_attr(target_board = "gimletlet-1", path = "bsp/gimletlet_mgmt.rs")]
#[cfg_attr(
    all(target_board = "gimletlet-2", feature = "gimletlet-nic"),
    path = "bsp/gimletlet_nic.rs"
)]
#[cfg_attr(target_board = "medusa-a", path = "bsp/medusa_a.rs")]
#[cfg_attr(target_board = "grapefruit", path = "bsp/grapefruit.rs")]
#[cfg_attr(target_board = "minibar", path = "bsp/minibar.rs")]
#[cfg_attr(target_board = "cosmo-a", path = "bsp/cosmo_a.rs")]
mod bsp;

#[cfg_attr(feature = "vlan", path = "server_vlan.rs")]
#[cfg_attr(not(feature = "vlan"), path = "server_basic.rs")]
mod server_impl;

#[cfg(feature = "mgmt")]
pub(crate) mod mgmt;

mod idl {
    use task_net_api::{
        KszError, KszMacTableEntry, LargePayloadBehavior, MacAddress,
        MacAddressBlock, ManagementCounters, ManagementLinkStatus, MgmtError,
        PhyError, SocketName, UdpMetadata, VLanId,
    };
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

use enum_map::Enum;
use multitimer::{Multitimer, Repeat};
use task_net_api::MacAddressBlock;
use zerocopy::{AsBytes, U16};

#[cfg(feature = "h743")]
use stm32h7::stm32h743 as device;
#[cfg(feature = "h753")]
use stm32h7::stm32h753 as device;

use counters::*;
use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::Sys;
use userlib::*;

use crate::bsp::BspImpl;
use crate::bsp_support::Bsp;

task_slot!(SYS, sys);
task_slot!(JEFE, jefe);

#[cfg(feature = "vpd-mac")]
task_slot!(PACKRAT, packrat);

/////////////////////////////////////////////////////////////////////////////
// Configuration things!
//
// Much of this needs to move into the board-level configuration.

/// Calculates a locally administered, unicast MAC address from the chip ID
///
/// This uses a hash of the chip ID and returns a block with starting MAC
/// address of the form `0e:1d:XX:XX:XX:XX`.  The MAC address block has a stride
/// of 1 and contains `VLanId::LENGTH` MAC addresses (or 1, if we're running
/// without VLANs enabled).
fn mac_address_from_uid(sys: &Sys) -> MacAddressBlock {
    let mut buf = [0u8; 6];
    let uid = sys.read_uid();
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

    MacAddressBlock {
        base_mac: buf,
        count: U16::new(crate::generated::PORT_COUNT.try_into().unwrap()),
        stride: 1,
    }
}

#[cfg(feature = "vpd-mac")]
fn mac_address_from_vpd() -> Option<MacAddressBlock> {
    // The first nontrivial thing `main()` does is call `BspImpl::preinit()`,
    // which waits for the sequencer task on all our major boards to progress to
    // an appropriate point, which includes having read board VPD and loaded it
    // into packrat, so we don't need to wait here.
    use task_packrat_api::Packrat;
    let packrat = Packrat::from(PACKRAT.get_task_id());
    packrat.get_mac_address_block().ok()
}

////////////////////////////////////////////////////////////////////////////////

const TX_RING_SZ: usize = 4;

const RX_RING_SZ: usize = 4;

/////////////////////////////////////////////////////////////////////////////
// Main driver loop.

#[derive(Count)]
enum Event {
    /// Global count of passes through the driver loop, for inspection through a
    /// debugger.
    Iter,
    /// IP activity occurred on an iteration.
    IpActivity,
    /// Timer wakeups
    TimerWake,
}
counters!(Event);

#[export_name = "main"]
fn main() -> ! {
    let sys = SYS.get_task_id();
    let sys = Sys::from(sys);

    // Do any preinit tasks specific to this board.  For hardware which requires
    // explicit clock configuration, this is where the `net` tasks waits for
    // the clock to come up.
    BspImpl::preinit();

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
    BspImpl::configure_ethernet_pins(&sys);

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
        notifications::MDIO_TIMER_IRQ_MASK,
    );

    // Set up the network stack.
    #[cfg(feature = "vpd-mac")]
    let mac_address =
        mac_address_from_vpd().unwrap_or_else(|| mac_address_from_uid(&sys));

    #[cfg(not(feature = "vpd-mac"))]
    let mac_address = mac_address_from_uid(&sys);

    // Board-dependant initialization (e.g. bringing up the PHYs)
    let bsp = BspImpl::new(&eth, &sys);

    let mut server = server_impl::new(&eth, mac_address, bsp);

    // Turn on our IRQ.
    userlib::sys_irq_control(notifications::ETH_IRQ_MASK, true);

    // We use only one timer, but we're using a multitimer in case we need to
    // add a second one again (we previously had a second one for the watchdog):
    #[derive(Copy, Clone, Enum)]
    enum Timers {
        Wake,
    }
    let mut multitimer =
        Multitimer::<Timers>::new(notifications::WAKE_TIMER_BIT);

    let now = sys_get_timer().now;
    if let Some(wake_interval) = BspImpl::WAKE_INTERVAL {
        // Some of the BSPs include a 'wake' function which allows for periodic
        // logging.  We schedule a wake-up before entering the idol_runtime
        // dispatch loop, to make sure that this gets called periodically.
        multitimer.set_timer(
            Timers::Wake,
            now,
            Some(Repeat::AfterWake(wake_interval)),
        );
    }

    // Ensure that sockets are woken at least once at startup, so that anyone
    // who was waiting to hear back on their TX queue becoming non-full will
    // snap out of it.
    //
    // This only works because we've set waiting_to_send to true for all sockets
    // above.
    server.wake_sockets();

    // Go!
    loop {
        count!(Event::Iter);

        // Call into smoltcp.
        let now = sys_get_timer().now;
        let activity = server.poll(now);

        if activity.ip {
            count!(Event::IpActivity);
            // Ask the server to iterate over sockets looking for work
            server.wake_sockets();
        } else {
            multitimer.poll_now();
            for t in multitimer.iter_fired() {
                count!(Event::TimerWake);
                match t {
                    Timers::Wake => {
                        server.wake();
                        // timer is set to auto-repeat
                    }
                }
            }
            let mut msgbuf = [0u8; idl::INCOMING_SIZE];
            idol_runtime::dispatch(&mut msgbuf, &mut server);
        }
    }
}

/// Struct used to describe any activity during `poll`.
pub(crate) struct Activity {
    /// Did the IP stack do anything? (i.e. do we need to process socket events)
    ip: bool,
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

fn ethernet_capabilities(
    eth: &drv_stm32h7_eth::Ethernet,
) -> smoltcp::phy::DeviceCapabilities {
    let mut caps = smoltcp::phy::DeviceCapabilities::default();
    caps.max_transmission_unit = 1514;
    caps.max_burst_size = Some(1514 * eth.max_tx_burst_len());

    // We do not rely on _any_ of the IP checksum features, so we can leave
    // caps.checksum at default.

    caps
}

// Place to namespace all the bits generated by our config processor.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/net_config.rs"));
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
