// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use static_cell::ClaimOnceCell;
use task_temperance_demo_api::{
    AddError, AddRequest, BULK_MAX_LEN, CHECKSUM_MAX_LEN, FillRequest,
    MaxLenByteSlice, all_rpcs, demo, demo_bulk, demo_upload,
};
use temperance::{Request, autobuffer};
use temperance_ipc::{IpcBackend, IpcServer};
use userlib::{sys_get_timer, sys_set_timer};

// TODO: Add comment
macro_rules! log {
    ($($arg:tt)*) => {
        #[cfg(not(target_os = "none"))]
        eprintln!("demo-server: {}", format_args!($($arg)*));
        #[cfg(target_os = "none")]
        let _ = format_args!($($arg)*);
    };
}

autobuffer!(ServerBufs, all_rpcs);

static BUFS: ClaimOnceCell<ServerBufs> = ClaimOnceCell::new(ServerBufs::new());

const TIMER_INTERVAL: u64 = 500;

static SCRATCH: ClaimOnceCell<[u8; BULK_MAX_LEN]> =
    ClaimOnceCell::new([0; BULK_MAX_LEN]);

struct ServerImpl {
    total: u64,
    served: u32,
    scratch: &'static mut [u8; BULK_MAX_LEN],
}

fn bulk(bytes: &[u8]) -> MaxLenByteSlice<'_, BULK_MAX_LEN> {
    let len = bytes.len().min(BULK_MAX_LEN);
    MaxLenByteSlice::try_from(&bytes[..len]).unwrap()
}

impl demo_upload::Server for ServerImpl {
    fn sum<'rqst>(
        &mut self,
        rqst: Request<'_, MaxLenByteSlice<'rqst, BULK_MAX_LEN>>,
    ) -> u32 {
        self.served += 1;
        rqst.body
            .iter()
            .fold(0u32, |acc, &b| acc.wrapping_add(u32::from(b)))
    }
}

impl demo_bulk::Server for ServerImpl {
    fn fill<'rqst, 'resp>(
        &'resp mut self,
        rqst: Request<'_, FillRequest>,
    ) -> MaxLenByteSlice<'resp, BULK_MAX_LEN> {
        self.served += 1;
        let len = usize::from(rqst.body.len).min(BULK_MAX_LEN);
        self.scratch[..len].fill(rqst.body.byte);
        bulk(&self.scratch[..len])
    }

    fn echo<'rqst, 'resp>(
        &'resp mut self,
        rqst: Request<'_, MaxLenByteSlice<'rqst, BULK_MAX_LEN>>,
    ) -> MaxLenByteSlice<'resp, BULK_MAX_LEN> {
        self.served += 1;
        let len = rqst.body.len().min(BULK_MAX_LEN);
        self.scratch[..len].copy_from_slice(&rqst.body[..len]);
        bulk(&self.scratch[..len])
    }
}

impl demo::Server for ServerImpl {
    fn add<'rqst>(
        &mut self,
        rqst: Request<'_, AddRequest>,
    ) -> Result<u32, AddError> {
        self.served += 1;
        let AddRequest { a, b } = *rqst.body;
        a.checked_add(b).ok_or(AddError::Overflow)
    }

    fn bump<'rqst>(&mut self, rqst: Request<'_, u32>) -> u64 {
        self.served += 1;
        self.total += u64::from(*rqst.body);
        self.total
    }

    fn checksum<'rqst>(
        &mut self,
        rqst: Request<'_, MaxLenByteSlice<'rqst, CHECKSUM_MAX_LEN>>,
    ) -> u32 {
        self.served += 1;
        rqst.body
            .iter()
            .fold(0u32, |acc, &b| acc.rotate_left(5) ^ u32::from(b))
    }
}

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    let mut server = ServerImpl {
        total: 0,
        served: 0,
        scratch: SCRATCH.claim(),
    };
    let mut backend = IpcBackend::new(
        IpcServer::new(notifications::TIMER_MASK),
        BUFS.claim(),
    );

    log!("started");
    sys_set_timer(
        Some(sys_get_timer().now + TIMER_INTERVAL),
        notifications::TIMER_MASK,
    );

    backend.serve_forever(
        &mut server,
        all_rpcs::Server::serve_one,
        |s, bits| {
            if bits & notifications::TIMER_MASK != 0 {
                log!("served {} requests", s.served);
                sys_set_timer(
                    Some(sys_get_timer().now + TIMER_INTERVAL),
                    notifications::TIMER_MASK,
                );
            }
        },
    )
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
