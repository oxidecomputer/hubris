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

    let tx_bytes: [u8; 4] = [1, 2, 3, 4];
    let meta = UdpMetadata {
        // IPv6 multicast?
        addr: Address::Ipv4(Ipv4Address([255, 255, 255, 255])),
        port: 8,
        size: tx_bytes.len() as u32,
    };

    loop {
        hl::sleep_for(500);
        net.send_packet(SOCKET, meta, &tx_bytes).unwrap();
        UDP_BROADCAST_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

static UDP_BROADCAST_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
