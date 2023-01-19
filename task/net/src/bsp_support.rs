// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common bits for Board Support Packages (BSPs).
//!
//! This would be named `bsp` but the BSP infrastructure squats on that name.
//!
//! # How the BSP stuff works
//!
//! The netstack defines a `bsp` module, but chooses which source file to use
//! based on the board ID/rev, selecting among options in `src/bsp/`.
//!
//! A BSP module is expected to export a single type, called `BspImpl`, which
//! implements the `Bsp` trait from this module.

use drv_stm32h7_eth as eth;
use drv_stm32xx_sys_api::Sys;
use task_net_api::PhyError;
use vsc7448_pac::types::PhyRegisterAddress;

////////////////////////////////////////////////////////////////////////////////

cfg_if::cfg_if! {
    // Select local vs server SPI communication
    if #[cfg(all(feature = "ksz8463", feature = "use-spi-core"))] {
        // The SPI peripheral is owned by this task!
        pub type Ksz8463 =
            ksz8463::Ksz8463<drv_stm32h7_spi_server_core::SpiServerCore>;

        /// Claims the SPI core.
        ///
        /// This function can only be called once, and will panic otherwise!
        pub fn claim_spi(sys: &Sys) -> drv_stm32h7_spi_server_core::SpiServerCore {
            drv_stm32h7_spi_server_core::declare_spi_core!(
                sys.clone(), notifications::SPI_IRQ_MASK)
        }
    } else if #[cfg(all(feature = "ksz8463", not(feature = "use-spi-core")))] {
        // The SPI peripheral is owned by a separate `stm32h7-spi-server` task
        userlib::task_slot!(SPI, spi_driver);
        pub type Ksz8463 = ksz8463::Ksz8463<drv_spi_api::Spi>;

        /// Claims the SPI handle
        pub fn claim_spi(_sys: &Sys) -> drv_spi_api::Spi {
            drv_spi_api::Spi::from(SPI.get_task_id())
        }
    }
}

////////////////////////////////////////////////////////////////////////////////

#[cfg(feature = "mgmt")]
use task_net_api::MgmtError;

/// Operations that must be provided by a BSP for the netstack.
///
/// A module implementing a BSP is expected to expose a type called `BspImpl`
/// that implements this trait.
pub trait Bsp: Sized {
    /// How long to wait between calls to `wake`. `None` tells the netstack to
    /// never call `wake`.
    ///
    /// The default is `None`, which goes along with the default impl for
    /// `wake`. If you change one, change the other.
    const WAKE_INTERVAL: Option<u64> = None;

    /// Opportunity to do any work before the Ethernet peripheral is turned on.
    /// By default this does nothing, override it if necessary.
    fn preinit() {}
    /// Stateless function to configure ethernet pins before the Bsp struct
    /// is actually constructed
    fn configure_ethernet_pins(sys: &Sys);

    fn new(eth: &eth::Ethernet, sys: &Sys) -> Self;

    /// Pokes the board-specific code to do some sort of action periodically.
    /// The interval between calls to `wake` is defined by `WAKE_INTERVAL`; if
    /// it's `None`, this function won't be called.
    ///
    /// The default implementation of this function panics, which goes great
    /// with a `WAKE_INTERVAL` of `None`.
    fn wake(&self, _eth: &eth::Ethernet) {
        panic!();
    }

    fn phy_read(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        eth: &eth::Ethernet,
    ) -> Result<u16, PhyError>;

    fn phy_write(
        &mut self,
        port: u8,
        reg: PhyRegisterAddress<u16>,
        value: u16,
        eth: &eth::Ethernet,
    ) -> Result<(), PhyError>;

    #[cfg(feature = "ksz8463")]
    fn ksz8463(&self) -> &Ksz8463;

    #[cfg(feature = "mgmt")]
    fn management_link_status(
        &self,
        eth: &eth::Ethernet,
    ) -> Result<task_net_api::ManagementLinkStatus, MgmtError>;

    #[cfg(feature = "mgmt")]
    fn management_counters(
        &self,
        eth: &crate::eth::Ethernet,
    ) -> Result<task_net_api::ManagementCounters, MgmtError>;
}
