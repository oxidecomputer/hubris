// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use hubpack::SerializedSize;
use serde::Serialize;
use task_net_api::*;
use task_packrat_api::{Packrat, OxideIdentity};
use userlib::*;

#[cfg(feature = "vlan")]
use enum_map::Enum;

task_slot!(NET, net);
task_slot!(PACKRAT, packrat);

#[derive(Debug, Clone, Copy, Serialize, SerializedSize)]
struct BroadcastData {
    // Version for this data structure; adding new fields to the end is okay,
    // but changing the order, size, or meaning of existing fields should result
    // in a version bump.
    version: u32,

    mac_address: [u8; 6],
    image_id: [u8; 8],

    // If true, we have identity from our VPD, and the following three fields
    // are populated. If false, we have no VPD or failed to read it, and the
    // following three fields will be all zero.
    identity_valid: bool,
    part_number: [u8; OxideIdentity::PART_NUMBER_LEN],
    revision: u32,
    serial: [u8; OxideIdentity::SERIAL_LEN],
}

impl BroadcastData {
    const CURRENT_VERSION: u32 = 1;
}

// Ensure our serialized size doesn't change unexpectedly: if you land here
// because compilation has failed, consider whether you need to update
// `BroadcastData::CURRENT_VERSION`!
//
// Current size is 45 bytes:
// version (4)
// mac_address (6)
// image_id (8)
// identity_valid (1)
// part_number (11)
// revision (4)
// serial (11)
static_assertions::const_assert_eq!(BroadcastData::MAX_SIZE, 45);

#[export_name = "main"]
fn main() -> ! {
    let net = NET.get_task_id();
    let net = Net::from(net);

    let packrat = PACKRAT.get_task_id();
    let packrat = Packrat::from(packrat);

    const SOCKET: SocketName = SocketName::broadcast;

    // If this system is running in VLAN mode, then we broadcast to each
    // possible VLAN in turn.  Otherwise, broadcast normal packets.
    #[cfg(feature = "vlan")]
    let mut vid_iter = (0..VLanId::LENGTH).map(VLanId::from_usize).cycle();

    // Ask `net` for our mac address first; this also serves as a useful wait
    // for `packrat` to be loaded by the sequencer if we're on a board with VPD.
    let mac_address = net.get_mac_address().0;

    // If we're on a board with no VPD or VPD reading failed, we'll construct a
    // default (all 0) identity and set `identity_valid` to false.
    let identity = packrat.get_identity().ok();
    let identity_valid = identity.is_some();
    let identity = identity.unwrap_or_default();

    let data = BroadcastData {
        version: BroadcastData::CURRENT_VERSION,
        mac_address,
        image_id: kipc::read_image_id().to_le_bytes(),
        identity_valid,
        part_number: identity.part_number,
        revision: identity.revision,
        serial: identity.serial,
    };

    #[cfg(feature = "vlan")]
    let sleep_time = (1000 / VLanId::LENGTH) as u64;

    #[cfg(not(feature = "vlan"))]
    let sleep_time = 1000;

    let mut out = [0u8; BroadcastData::MAX_SIZE];
    let n = hubpack::serialize(&mut out, &data).unwrap_lite();
    let out = &out[..n];

    loop {
        let meta = UdpMetadata {
            // IPv6 multicast address for "all nodes"
            addr: Address::Ipv6(Ipv6Address([
                0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            ])),
            port: 8888,
            size: out.len() as u32,
            #[cfg(feature = "vlan")]
            vid: vid_iter.next().unwrap(),
        };

        hl::sleep_for(sleep_time);
        match net.send_packet(SOCKET, meta, out) {
            Ok(()) => UDP_BROADCAST_COUNT
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            Err(_) => UDP_ERROR_COUNT
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed),
        };
    }
}

static UDP_BROADCAST_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
static UDP_ERROR_COUNT: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);
