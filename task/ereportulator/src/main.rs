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
//! ```
//!
//! In addition, when testing on systems which lack real vital product data
//! (VPD) EEPROMs, such as on Gimletlet, this task can be configured to send a
//! made-up VPD identity to packrat upon startup, so that ereports generated in
//! testing can have realistic-looking VPD metadata. This is enabled by the
//! "fake-vpd" feature flag.
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
    #[cfg(feature = "fake-vpd")]
    VpdAlreadySet,
    #[cfg(feature = "fake-vpd")]
    SetFakeVpd,

    EreportRequested(u32),
    EreportDelivered {
        encoded_len: usize,
    },
}

counted_ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let packrat = Packrat::from(PACKRAT.get_task_id());

    #[cfg(feature = "fake-vpd")]
    fake_vpd(&packrat);

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

        self.packrat.deliver_ereport(&self.buf[..encoded_len]);
        ringbuf_entry!(Trace::EreportDelivered { encoded_len });

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

#[cfg(feature = "fake-vpd")]
fn fake_vpd(packrat: &Packrat) {
    // If someone else has already set identity, just don't clobber it.
    if packrat.get_identity().is_ok() {
        ringbuf_entry!(Trace::VpdAlreadySet);
        return;
    }

    // Just make up some nonsense.
    packrat
        .set_identity(task_packrat_api::VpdIdentity {
            part_number: *b"LOLNO000000",
            serial: *b"69426661337",
            revision: 42,
        })
        .unwrap_lite();

    ringbuf_entry!(Trace::SetFakeVpd);
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}
