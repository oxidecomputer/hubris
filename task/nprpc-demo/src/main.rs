// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use static_cell::ClaimOnceCell;
use task_net_api::{LargePayloadBehavior, SocketName, UdpMetadata};
use userlib::{TaskId, sys_recv, task_slot};

use nprpc::{
    ServerInterfaceError, autobuffer,
    io::server::{Backend, RawInterfaceFrame},
};
use udp_api::MaxLenStr;

task_slot!(NET, net);

// Automatically sized buffers
autobuffer!(ApiBufs, udp_api::composite);

struct ServerImpl {
    name: Option<heapless::String<32>>,
}

// Implement the "hello" interface
impl udp_api::hello::Server for ServerImpl {
    fn loopback(&mut self, req: nprpc::Request<u32>) -> u32 {
        req.req
    }
}

// Implement the "kv" interface
impl udp_api::kv::Server for ServerImpl {
    fn set_name<'req, 'resp>(
        &'resp mut self,
        req: nprpc::Request<MaxLenStr<'req, 32>>,
    ) {
        let s: &str = &req.req;
        let s = heapless::String::<32>::from(s);
        self.name = Some(s);
    }

    fn get_name<'req, 'resp>(
        &'resp mut self,
        _req: nprpc::Request<()>,
    ) -> Option<MaxLenStr<'resp, 32>> {
        let s: &str = self.name.as_deref()?;
        MaxLenStr::try_from(s).ok()
    }
}

#[unsafe(export_name = "main")]
fn main() -> ! {
    static BUFS: ClaimOnceCell<ApiBufs> = ClaimOnceCell::new(ApiBufs::new());
    let net = task_net_api::Net::from(NET.get_task_id());
    let mut wire = Wire {
        socket: HubrisSocket(net),
        buffers: BUFS.claim(),
    };
    let mut server = ServerImpl { name: None };

    loop {
        // Get notification
        let rm = sys_recv(&mut [], notifications::SOCKET_MASK, None).unwrap();
        assert!(rm.sender == TaskId::KERNEL);

        // TODO: ringbuf on errors
        let _ = wire.serve_one(|reqraw, outgoing| {
            <ServerImpl as udp_api::composite::Server>::process_one(
                &mut server,
                reqraw,
                outgoing,
            )
        });
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));

/////
// TODO: Items below here should probably be in `nprpc` (or in a hubris crate)
// as standard interface impls.
//

struct HubrisSocket(task_net_api::Net);

struct Wire {
    socket: HubrisSocket,
    buffers: &'static mut ApiBufs,
}

impl nprpc::io::server::Interface for HubrisSocket {
    type Meta = UdpMetadata;

    type Error = ();

    fn recv_one_frame_raw<'data>(
        &mut self,
        incoming: &'data mut [u8],
    ) -> Result<
        Option<RawInterfaceFrame<'data, Self::Meta>>,
        ServerInterfaceError<Self::Error>,
    > {
        let Ok(meta) = self.0.recv_packet(
            SocketName::nprpc,
            LargePayloadBehavior::Discard,
            incoming,
        ) else {
            // TODO: ringbuf errors
            return Ok(None);
        };
        Ok(Some(RawInterfaceFrame {
            meta,
            raw: &incoming[..(meta.size as usize)],
        }))
    }

    fn send_one_frame_raw(
        &mut self,
        outgoing: RawInterfaceFrame<'_, Self::Meta>,
    ) -> Result<(), ServerInterfaceError<Self::Error>> {
        let RawInterfaceFrame { mut meta, raw } = outgoing;
        meta.size = raw.len() as u32;
        // TODO: plumb/ringbuf errors
        _ = self.0.send_packet(SocketName::nprpc, meta, raw);
        Ok(())
    }
}

impl nprpc::io::server::Backend for Wire {
    type Storage = ApiBufs;
    type Interface = HubrisSocket;

    fn parts(&mut self) -> (&mut Self::Storage, &mut Self::Interface) {
        let Self { socket, buffers } = self;
        (buffers, socket)
    }
}
