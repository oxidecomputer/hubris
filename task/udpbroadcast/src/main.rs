// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use task_net_api::*;
use userlib::*;

task_slot!(NET, net);

#[export_name = "main"]
fn main() -> ! {
    let net = NET.get_task_id();
    let net = Net::from(net);

    const SOCKET: SocketName = SocketName::broadcast;

    // If this system is running in VLAN mode, then we broadcast to each
    // possible VLAN in turn.  Otherwise, broadcast normal packets.
    #[cfg(feature = "vlan")]
    let mut vid_iter = VLAN_RANGE.cycle();

    loop {
        let tx_bytes: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let meta = UdpMetadata {
            // IPv6 multicast address for "all nodes"
            addr: Address::Ipv6(Ipv6Address([
                0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            ])),
            port: 8,
            size: tx_bytes.len() as u32,
            #[cfg(feature = "vlan")]
            vid: vid_iter.next().unwrap(),
        };

        hl::sleep_for(500);
        match net.send_packet(SOCKET, meta, &tx_bytes) {
            Ok(_) => UDP_BROADCAST_COUNT
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            Err(_) => UDP_ERROR_COUNT
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        };
    }
}

static UDP_ERROR_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
static UDP_BROADCAST_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
