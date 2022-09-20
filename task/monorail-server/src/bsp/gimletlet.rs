// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use drv_fpga_api::{FpgaError, WriteOp};
use drv_spi_api::{Spi, SpiDevice};
use userlib::{hl::sleep_for, task_slot};
use vsc7448::Vsc7448;
use vsc7448::VscError;
use vsc85xx::{vsc8562::Vsc8562Phy, PhyRw};
use zerocopy::{byteorder, AsBytes, FromBytes, Unaligned, U16};

task_slot!(SPI, spi_driver);

/// Interval at which `Bsp::wake()` is called by the main loop
pub const WAKE_INTERVAL: Option<u64> = None;

////////////////////////////////////////////////////////////////////////////////

pub struct Bsp<'a, R> {
    vsc8562: SpiPhyRw,
    _p: core::marker::PhantomData<&'a R>,
}

pub const REFCLK_SEL: vsc7448::RefClockFreq =
    vsc7448::RefClockFreq::Clk156p25MHz;
pub const REFCLK2_SEL: Option<vsc7448::RefClockFreq> = None;

mod map {
    use vsc7448::config::PortMap;
    pub const PORT_MAP: PortMap = PortMap::new([
        None, None, None, None, None, None, None, None, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None, None, None,
        None, None, None, None, None, None, None, None, None, None, None, None,
        None, None, None, None, None,
    ]);
}
pub use map::PORT_MAP;

pub fn preinit() {
    // Nothing to do here
}

impl<'a, R> Bsp<'a, R> {
    /// Constructs and initializes a new BSP handle
    pub fn new(_vsc7448: &'a Vsc7448<'a, R>) -> Result<Self, VscError> {
        let mut out = Bsp {
            vsc8562: SpiPhyRw::new(SPI.get_task_id()),
            _p: core::marker::PhantomData,
        };
        out.init()?;
        Ok(out)
    }

    fn init(&mut self) -> Result<(), VscError> {
        self.phy_vsc8562_init()?;

        Ok(())
    }

    pub fn phy_vsc8562_init(&mut self) -> Result<(), VscError> {
        let phy_rw = &mut self.vsc8562;

        // Do a hard power cycle of the PHY; the fine details of sequencing
        // are handled by the FPGA
        phy_rw
            .set_phy_power_enabled(false)
            .map_err(|e| VscError::ProxyError(e.into()))?;
        sleep_for(10);
        phy_rw
            .set_phy_power_enabled(true)
            .map_err(|e| VscError::ProxyError(e.into()))?;
        while !phy_rw
            .phy_powered_up_and_ready()
            .map_err(|e| VscError::ProxyError(e.into()))?
        {
            sleep_for(20);
        }
        for p in 0..2 {
            let mut phy = vsc85xx::Phy::new(p, phy_rw);
            let mut v = Vsc8562Phy { phy: &mut phy };
            v.init_qsgmii()?;
        }
        phy_rw
            .set_phy_coma_mode(false)
            .map_err(|e| VscError::ProxyError(e.into()))?;

        Ok(())
    }

    pub fn wake(&mut self) -> Result<(), VscError> {
        panic!()
    }

    /// Calls a function on a `Phy` associated with the given port.
    ///
    /// Returns `None` if the given port isn't associated with a PHY
    /// (for example, because it's an SGMII link)
    pub fn phy_fn<T, F: Fn(vsc85xx::Phy<'_, SpiPhyRw>) -> T>(
        &mut self,
        _port: u8,
        _callback: F,
    ) -> Option<T> {
        panic!()
    }
}

////////////////////////////////////////////////////////////////////////////////

pub struct SpiPhyRw {
    spi: SpiDevice,
    await_not_busy_sleep_for: u64,
}

#[allow(non_snake_case, dead_code)]
mod Addr {
    pub const PHY_STATUS: u16 = 16;
    pub const PHY_CTRL: u16 = 17;
    pub const PHY_SMI_STATUS: u16 = 18;
    pub const PHY_SMI_RDATA_H: u16 = 19;
    pub const PHY_SMI_RDATA_L: u16 = 20;
    pub const PHY_SMI_WDATA_H: u16 = 21;
    pub const PHY_SMI_WDATA_L: u16 = 22;
    pub const PHY_SMI_PHY_ADDR: u16 = 23;
    pub const PHY_SMI_REG_ADDR: u16 = 24;
    pub const PHY_SMI_CTRL: u16 = 25;
}

#[allow(non_snake_case, dead_code)]
mod Reg {
    pub mod PHY_SMI_CTRL {
        pub const START: u8 = 1 << 1;
        pub const RW: u8 = 1 << 0;
    }
    pub mod PHY_SMI_STATUS {
        pub const BUSY: u8 = 1 << 0;
    }
    pub mod PHY_STATUS {
        pub const READY: u8 = 1 << 6;
    }
    pub mod PHY_CTRL {
        pub const COMA_MODE: u8 = 1 << 1;
        pub const EN: u8 = 1 << 0;
    }
}

#[derive(AsBytes, FromBytes, Unaligned)]
#[repr(C)]
struct SmiRequest {
    rdata: U16<byteorder::BigEndian>,
    wdata: U16<byteorder::BigEndian>,
    phy: u8,
    reg: u8,
    ctrl: u8,
}

impl SpiPhyRw {
    pub fn new(spi_task: userlib::TaskId) -> Self {
        Self {
            spi: SpiDevice::new(Spi::from(spi_task), 0),
            await_not_busy_sleep_for: 0,
        }
    }

    pub fn fpga_read<T>(&self, addr: impl Into<u16>) -> Result<T, FpgaError>
    where
        T: AsBytes + Default + FromBytes,
    {
        let header = UserDesignRequestHeader {
            cmd: 0x1,
            addr: U16::new(addr.into()),
        };
        let mut out = T::default();
        self.spi.lock(drv_spi_api::CsState::Asserted).unwrap();
        self.spi.write(header.as_bytes()).unwrap();
        self.spi.read(out.as_bytes_mut()).unwrap();
        self.spi.release().unwrap();
        Ok(out)
    }

    pub fn fpga_write<T>(
        &self,
        op: WriteOp,
        addr: impl Into<u16>,
        value: T,
    ) -> Result<(), FpgaError>
    where
        T: AsBytes + FromBytes,
    {
        let header = UserDesignRequestHeader {
            cmd: u8::from(op),
            addr: U16::new(addr.into()),
        };

        self.spi.lock(drv_spi_api::CsState::Asserted).unwrap();
        self.spi.write(header.as_bytes()).unwrap();
        self.spi.write(value.as_bytes()).unwrap();
        self.spi.release().unwrap();
        Ok(())
    }

    #[inline]
    pub fn set_phy_power_enabled(
        &self,
        enabled: bool,
    ) -> Result<(), FpgaError> {
        self.fpga_write(
            WriteOp::from(enabled),
            Addr::PHY_CTRL,
            Reg::PHY_CTRL::EN,
        )
    }

    #[inline]
    pub fn set_phy_coma_mode(&self, asserted: bool) -> Result<(), FpgaError> {
        self.fpga_write(
            WriteOp::from(asserted),
            Addr::PHY_CTRL,
            Reg::PHY_CTRL::COMA_MODE,
        )
    }

    #[inline]
    pub fn phy_powered_up_and_ready(&self) -> Result<bool, FpgaError> {
        let status: u8 = self.fpga_read(Addr::PHY_STATUS)?;
        Ok((status & Reg::PHY_STATUS::READY) != 0)
    }

    #[inline]
    pub fn smi_busy(&self) -> Result<bool, FpgaError> {
        let status: u8 = self.fpga_read(Addr::PHY_SMI_STATUS)?;
        Ok((status & Reg::PHY_SMI_STATUS::BUSY) != 0)
    }

    pub fn await_not_busy(&self) -> Result<(), FpgaError> {
        while self.smi_busy()? {
            if self.await_not_busy_sleep_for > 0 {
                userlib::hl::sleep_for(self.await_not_busy_sleep_for);
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn read_raw_inner(&self, phy: u8, reg: u8) -> Result<u16, FpgaError> {
        let request = SmiRequest {
            rdata: U16::new(0),
            wdata: U16::new(0),
            phy,
            reg,
            ctrl: Reg::PHY_SMI_CTRL::START,
        };
        self.await_not_busy()?;
        self.fpga_write(WriteOp::Write, Addr::PHY_SMI_RDATA_H, request)?;

        self.await_not_busy()?;
        let v = u16::from_be(self.fpga_read(Addr::PHY_SMI_RDATA_H)?);

        Ok(v)
    }

    #[inline(never)]
    fn write_raw_inner(
        &self,
        phy: u8,
        reg: u8,
        value: u16,
    ) -> Result<(), FpgaError> {
        let request = SmiRequest {
            rdata: U16::new(0),
            wdata: U16::new(value),
            phy,
            reg,
            ctrl: Reg::PHY_SMI_CTRL::RW | Reg::PHY_SMI_CTRL::START,
        };
        self.await_not_busy()?;
        self.fpga_write(WriteOp::Write, Addr::PHY_SMI_RDATA_H, request)
    }
}

impl PhyRw for SpiPhyRw {
    #[inline(always)]
    fn read_raw(&self, phy: u8, reg: u8) -> Result<u16, VscError> {
        self.read_raw_inner(phy, reg)
            .map_err(|e| VscError::ProxyError(e.into()))
    }

    #[inline(always)]
    fn write_raw(&self, phy: u8, reg: u8, value: u16) -> Result<(), VscError> {
        self.write_raw_inner(phy, reg, value)
            .map_err(|e| VscError::ProxyError(e.into()))
    }
}

#[derive(AsBytes, Unaligned)]
#[repr(C)]
struct UserDesignRequestHeader {
    cmd: u8,
    addr: U16<byteorder::BigEndian>,
}
