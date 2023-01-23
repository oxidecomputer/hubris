// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

#[cfg_attr(target_board = "sidecar-a", path = "bsp/sidecar_a.rs")]
#[cfg_attr(target_board = "sidecar-b", path = "bsp/sidecar_b.rs")]
#[cfg_attr(
    any(target_board = "gemini-bu-1", target_board = "gimletlet-2"),
    path = "bsp/vsc7448_dev.rs"
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
            // Note that this *always* maps the SPI interrupt to interrupt mask
            // 0b1, which must match the TOML file.
            drv_stm32h7_spi_server_core::declare_spi_core!(sys.clone(), 1)
        }
    } else {
        pub fn claim_spi(_sys: &Sys) -> drv_spi_api::Spi {
            task_slot!(SPI, spi_driver);
            drv_spi_api::Spi::from(SPI.get_task_id())
        }
    }
}

const VSC7448_SPI_DEVICE: u8 = 0;

task_slot!(SYS, sys);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    ChipInit(u64),
    ChipInitFailed(VscError),
    BspInit(u64),
    BspInitFailed(VscError),
    WakeErr(VscError),
}
ringbuf!(Trace, 2, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let sys = Sys::from(SYS.get_task_id());
    let spi = claim_spi(&sys).device(VSC7448_SPI_DEVICE);
    let mut vsc7448_spi = Vsc7448Spi::new(spi);
    let vsc7448 = Vsc7448::new(&mut vsc7448_spi);

    // Used to turn on LEDs before anything else happens
    bsp::preinit();

    let t0 = sys_get_timer().now;
    match vsc7448.init(bsp::REFCLK_SEL, bsp::REFCLK2_SEL) {
        Ok(()) => {
            let t1 = sys_get_timer().now;
            ringbuf_entry!(Trace::ChipInit(t1 - t0));
        }
        Err(e) => {
            ringbuf_entry!(Trace::ChipInitFailed(e));
            panic!("Could not initialize chip: {:?}", e);
        }
    }

    let t0 = sys_get_timer().now;
    let bsp = match Bsp::new(&vsc7448) {
        Ok(bsp) => {
            let t1 = sys_get_timer().now;
            ringbuf_entry!(Trace::BspInit(t1 - t0));
            bsp
        }
        Err(e) => {
            ringbuf_entry!(Trace::BspInitFailed(e));
            panic!("Could not initialize BSP: {:?}", e);
        }
    };

    let mut server = ServerImpl::new(bsp, &vsc7448, &bsp::PORT_MAP);
    loop {
        if let Err(e) = server.wake() {
            ringbuf_entry!(Trace::WakeErr(e));
        }
        let mut msgbuf = [0u8; server::INCOMING_SIZE];
        idol_runtime::dispatch_n(&mut msgbuf, &mut server);
    }
}
