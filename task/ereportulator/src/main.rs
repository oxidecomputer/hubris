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

use fixedstr::FixedStr;
use idol_runtime::RequestError;
use microcbor::Encode;
use ringbuf::{counted_ringbuf, ringbuf_entry};
use task_packrat_api::Packrat;
use userlib::{sys_get_timer, task_slot, RecvMessage};

task_slot!(PACKRAT, packrat);

#[derive(Copy, Clone, Eq, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,

    SetFakeVpd(#[count(children)] Result<(), task_packrat_api::CacheSetError>),
    EreportRequested {
        n: u32,
    },
    EreportDone {
        duration: u64,
    },
}

counted_ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let packrat = Packrat::from(PACKRAT.get_task_id());
    let mut server = ServerImpl {
        ereporter: Ereporter::claim_static_resources(packrat.clone()),
        packrat,
    };

    let mut buffer = [0; idl::INCOMING_SIZE];

    loop {
        idol_runtime::dispatch(&mut buffer, &mut server);
    }
}

struct ServerImpl {
    packrat: Packrat,
    ereporter: Ereporter,
}

impl idl::InOrderEreportulatorImpl for ServerImpl {
    fn fake_ereport(
        &mut self,
        _msg: &RecvMessage,
        n: u32,
    ) -> Result<(), RequestError<Infallible>> {
        let t0 = sys_get_timer().now;
        ringbuf_entry!(Trace::EreportRequested { n });

        self.ereporter.deliver_ereport(&TestEreportPlsIgnore {
            badness: n,
            msg: fixedstr::FixedStr::from_str("im dead"),
        });

        ringbuf_entry!(Trace::EreportDone {
            duration: sys_get_timer().now - t0
        });
        Ok(())
    }

    fn ae35_fault(
        &mut self,
        _msg: &RecvMessage,
        n: u32,
    ) -> Result<(), RequestError<Infallible>> {
        let t0 = sys_get_timer().now;
        ringbuf_entry!(Trace::EreportRequested { n });

        self.ereporter.deliver_ereport(&Ae35UnitEreport {
            critical_in_hrs: 32,
            detected_by: FixedStr::from_str("HAL-9000"),
            n,
        });

        ringbuf_entry!(Trace::EreportDone {
            duration: sys_get_timer().now - t0
        });
        Ok(())
    }

    fn houston_we_have_a_problem(
        &mut self,
        _msg: &RecvMessage,
        n: u32,
    ) -> Result<(), RequestError<Infallible>> {
        let t0 = sys_get_timer().now;
        ringbuf_entry!(Trace::EreportRequested { n });

        let ereport = if n.is_multiple_of(2) {
            // Not historically accurate...
            MainBusUndervoltEreport::MainBusB { volts: 0.00, n }
        } else {
            MainBusUndervoltEreport::MainBusA { volts: 0.01, n }
        };
        self.ereporter.deliver_ereport(&ereport);

        ringbuf_entry!(Trace::EreportDone {
            duration: sys_get_timer().now - t0
        });
        Ok(())
    }

    fn set_fake_vpd(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<task_packrat_api::CacheSetError>> {
        let result =
            self.packrat.set_identity(task_packrat_api::OxideIdentity {
                part_number: *b"LOLNO000000",
                serial: *b"69426661337",
                revision: 42,
            });

        ringbuf_entry!(Trace::SetFakeVpd(result));

        result?;

        Ok(())
    }

    fn panicme(
        &mut self,
        _msg: &RecvMessage,
    ) -> Result<(), RequestError<Infallible>> {
        panic!("im dead lol")
    }
}

impl idol_runtime::NotificationHandler for ServerImpl {
    fn current_notification_mask(&self) -> u32 {
        // We don't use notifications, don't listen for any.
        0
    }

    fn handle_notification(&mut self, _bits: userlib::NotificationBits) {
        unreachable!()
    }
}

mod idl {
    include!(concat!(env!("OUT_DIR"), "/server_stub.rs"));
}

ereports::declare_ereporter! {
    struct Ereporter<Ereport> {
        Ae35Fault(Ae35UnitEreport),
        MainBusUndervolt(MainBusUndervoltEreport),
        TestPlsIgnore(TestEreportPlsIgnore)
    }
}

#[derive(Encode)]
#[ereport(class = "hw.discovery-one.ae35.fault", version = 0)]
struct Ae35UnitEreport {
    critical_in_hrs: u32,
    detected_by: fixedstr::FixedStr<'static, 8>,
    n: u32,
}

#[derive(Encode)]
#[ereport(class = "hw.apollo.undervolt", version = 13)]
#[cbor(variant_id = "bus")]
enum MainBusUndervoltEreport {
    MainBusA { volts: f32, n: u32 },
    MainBusB { volts: f32, n: u32 }, // "Houston, we've got a main bus B undervolt!"
}

#[derive(Encode)]
#[ereport(class = "test.ereport.please.ignore", version = 420)]
struct TestEreportPlsIgnore {
    badness: u32,
    msg: fixedstr::FixedStr<'static, 8>,
}
