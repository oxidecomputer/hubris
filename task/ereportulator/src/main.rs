// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//!
//! # BEHOLD THE EREPORTULATOR!
//!
//! This is a demo/testing task for the ereport subsystem; it is not intended
//! to be included in production images. The ereportulator is a simple task for
//! generating fake ereports when requested via Hiffy, for testing purposes.
//!
//! Ereports are requested using the `Ereportulator.fake_ereport` IPC operation.
//! This takes one argument, `n`, which is an arbitrary `u32` value to include
//! in the ereport --- intended for differentiating between ereports generated
//! during a test.
//!
//! For example:
//!
//! ```console
//! $ humility -t gimletlet hiffy -c Ereportulator.fake_ereport -a n=420
//! humility: WARNING: archive on command-line overriding archive in environment file
//! humility: attached to 0483:3754:000B00154D46501520383832 via ST-Link V3
//! Ereportulator.fake_ereport() => ()
//!
//! ```
//!
//! In addition, when testing on systems which lack real vital product data
//! (VPD) EEPROMs, such as on Gimletlet, this task can be asked to send a
//! made-up VPD identity to packrat. This way, ereports generated in testing
//! can have realistic-looking VPD metadata. Fake VPD is requested using the
//! `Ereportulator.set_fake_vpd` IPC operation:
//!
//! ```console
//! $ humility -t gimletlet hiffy -c Ereportulator.set_fake_vpd
//! humility: WARNING: archive on command-line overriding archive in environment file
//! humility: attached to 0483:3754:000B00154D46501520383832 via ST-Link V3
//! Ereportulator.set_fake_vpd() => ()
//!
//! ```
//!
//!
#![no_std]
#![no_main]

use core::convert::Infallible;

use idol_runtime::RequestError;
use minicbor::Encoder;
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_packrat_api::Packrat;
use userlib::{task_slot, RecvMessage, UnwrapLite};

task_slot!(PACKRAT, packrat);

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,

    SetFakeVpd(#[count(children)] Result<(), task_packrat_api::CacheSetError>),
    EreportRequested(u32),
    EreportDelivered {
        encoded_len: usize,
    },
    EreportLost {
        encoded_len: usize,
        err: task_packrat_api::EreportWriteError,
    },
}

counted_ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let packrat = Packrat::from(PACKRAT.get_task_id());

    let mut server = ServerImpl {
        buf: [0; 256],
        packrat,
    };

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    buf: [u8; 256],
    packrat: Packrat,
}

impl idl::InOrderEreportulatorImpl for ServerImpl {
    fn fake_ereport(
        &mut self,
        _msg: &RecvMessage,
        n: u32,
    ) -> Result<(), RequestError<Infallible>> {
        ringbuf_entry!(Trace::EreportRequested(n));

        let encoded_len = {
            let c = minicbor::encode::write::Cursor::new(&mut self.buf[..]);
            let mut encoder = Encoder::new(c);

            // It's bad on purpose to make you click, Cliff!
            encoder
                .begin_map()
                .unwrap_lite()
                .str("k")
                .unwrap_lite()
                .str("test.ereport.please.ignore")
                .unwrap_lite()
                .str("badness")
                .unwrap_lite()
                .u32(n)
                .unwrap_lite()
                .str("msg")
                .unwrap_lite()
                .str("im dead")
                .unwrap_lite()
                .end()
                .unwrap_lite();

            encoder.into_writer().position()
        };

        match self.packrat.deliver_ereport(&self.buf[..encoded_len]) {
            Ok(_) => ringbuf_entry!(Trace::EreportDelivered { encoded_len }),
            Err(err) => ringbuf_entry!(Trace::EreportLost { encoded_len, err }),
        }

        Ok(())
    }

    fn set_fake_vpd(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<task_packrat_api::CacheSetError>> {
        let result = self.packrat.set_identity(task_packrat_api::OxideIdentity {
            part_number: *b"LOLNO000000",
            serial: *b"69426661337",
            revision: 42,
        });

        ringbuf_entry!(Trace::SetFakeVpd(result));

        result?;

        Ok(())
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: u32) {
        unreachable!()
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
