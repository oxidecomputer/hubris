// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg_attr(
    any(
        target_board = "sidecar-b",
        target_board = "sidecar-c",
        target_board = "sidecar-d"
    ),
    path = "bsp/sidecar_bcd.rs"
)]
#[cfg_attr(target_board = "medusa-a", path = "bsp/medusa_a.rs")]
#[cfg_attr(
    any(target_board = "minibar-a", target_board = "minibar-b"),
    path = "bsp/minibar.rs"
)]
mod bsp;
mod server;

use crate::{bsp::Bsp, server::ServerImpl};
use drv_spi_api::SpiServer;
use drv_stm32xx_sys_api::Sys;
use ringbuf::*;
use userlib::*;
use vsc7448::{spi::Vsc7448Spi, Vsc7448, VscError};

cfg_if::cfg_if! {
    // Select local vs server SPI communication
    if #[cfg(feature = "use-spi-core")] {
        /// Claims the SPI core.
        ///
        /// This function can only be called once, and will panic otherwise!
        pub fn claim_spi(sys: &Sys)
            -> drv_stm32h7_spi_server_core::SpiServerCore
        {
            drv_stm32h7_spi_server_core::declare_spi_core!(
                sys.clone(), notifications::SPI_IRQ_MASK)
        }
    } else {
        pub fn claim_spi(_sys: &Sys) -> drv_spi_api::Spi {
            task_slot!(SPI, spi_driver);
            drv_spi_api::Spi::from(SPI.get_task_id())
        }
    }
}

task_slot!(SYS, sys);

#[derive(Copy, Clone, PartialEq, counters::Count)]
enum Trace {
    #[count(skip)]
    None,
    BspInit(u64),
    BspInitFailed(#[count(children)] VscError),
    WakeErr(#[count(children)] VscError),
}
counted_ringbuf!(Trace, 2, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let sys = Sys::from(SYS.get_task_id());
    let spi = claim_spi(&sys).device(drv_spi_api::devices::VSC7448);
    let mut vsc7448_spi = Vsc7448Spi::new(spi);
    let vsc7448 =
        Vsc7448::new(&mut vsc7448_spi, bsp::REFCLK_SEL, bsp::REFCLK2_SEL);

    // Used to turn on LEDs before anything else happens
    bsp::preinit();

    let t0 = sys_get_timer().now;
    let bsp = match Bsp::new(&vsc7448) {
        Ok(bsp) => {
            let t1 = sys_get_timer().now;
            ringbuf_entry!(Trace::BspInit(t1 - t0));
            bsp
        }
        Err(e) => {
            // BSP initialization has failed. We intend to retry it. Restarting
            // the server is a convenient way of doing this.
            //
            // The first thing the BSP does when initialized is to assert the
            // reset line on the VSC7448, which will put it into a known state
            // for us to attempt initialization again. This _appears_ to reset
            // all state that could potentially have caused a panic within the
            // BSP, and in practice this panic occurs rarely and at most once.
            //
            // Writing the error into the ringbuf before panicking ensures that
            // it's available in the dump for inspection, in case we're curious.
            //
            // It's tempting to reach for the non-dumping task restart operation
            // (`Jefe::restart_me`) here, but it's probably not worth it for the
            // following reasons:
            //
            // 1. This happens infrequently -- it's not like we crash on every
            //    startup. We've seen this single-digit times so far.
            //
            // 2. Recording the dump in that infrequent case is actually useful,
            //    since it gets us the error that caused the panic. (Dumping the
            //    entire task burns 8 kiB to record a couple of bytes of error,
            //    but, only when the problem happens.)
            //
            // Should someone in the future want to remove this panic for
            // whatever reason, the _correct_ path is likely to retain the
            // ringbuf entry, and then wrap this code with a retry loop. This
            // will ensure that any errors are available for inspection (which
            // `Jefe::restart_me` would not) while making the restart cheaper.
            ringbuf_entry!(Trace::BspInitFailed(e));
            panic!();
        }
    };

    let mut server = ServerImpl::new(bsp, &vsc7448, &bsp::PORT_MAP);
    loop {
        if let Err(e) = server.wake() {
            ringbuf_entry!(Trace::WakeErr(e));
        }
        let mut msgbuf = [0u8; server::INCOMING_SIZE];
        idol_runtime::dispatch(&mut msgbuf, &mut server);
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
