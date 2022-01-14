use crate::{phy::PhyRw, spi::Vsc7448Spi, VscError};
use vsc7448_pac::{types::PhyRegisterAddress, Vsc7448};

pub struct Vsc7448SpiPhy<'a> {
    vsc7448: &'a Vsc7448Spi,
    miim: u32,
}

impl<'a> Vsc7448SpiPhy<'a> {
    pub fn new(vsc7448: &'a Vsc7448Spi, miim: u32) -> Self {
        Self { vsc7448, miim }
    }
    /// Builds a MII_CMD register based on the given phy and register.  Note
    /// that miim_cmd_opr_field is unset; you must configure it for a read
    /// or write yourself.
    fn miim_cmd(
        phy: u8,
        reg_addr: u8,
    ) -> vsc7448_pac::devcpu_gcb::miim::MII_CMD {
        let mut v: vsc7448_pac::devcpu_gcb::miim::MII_CMD = 0.into();
        v.set_miim_cmd_vld(1);
        v.set_miim_cmd_phyad(phy as u32);
        v.set_miim_cmd_regad(reg_addr as u32);
        v
    }

    /// Waits for the PENDING_RD and PENDING_WR bits to go low, indicating that
    /// it's safe to read or write to the MIIM.
    fn miim_idle_wait(&self) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .vsc7448
                .read(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_STATUS())?;
            if status.miim_stat_opr_pend() == 0 {
                return Ok(());
            }
        }
        return Err(VscError::MiimIdleTimeout);
    }

    /// Waits for the STAT_BUSY bit to go low, indicating that a read has
    /// finished and data is available.
    fn miim_read_wait(&self) -> Result<(), VscError> {
        for _i in 0..32 {
            let status = self
                .vsc7448
                .read(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_STATUS())?;
            if status.miim_stat_busy() == 0 {
                return Ok(());
            }
        }
        return Err(VscError::MiimReadTimeout);
    }
}

impl PhyRw for Vsc7448SpiPhy<'_> {
    fn read_raw<T: From<u16>>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError> {
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b10); // read

        self.miim_idle_wait()?;
        self.vsc7448
            .write(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_CMD(), v)?;
        self.miim_read_wait()?;

        let out = self
            .vsc7448
            .read(Vsc7448::DEVCPU_GCB().MIIM(self.miim).MII_DATA())?;
        if out.miim_data_success() == 0b11 {
            return Err(VscError::MiimReadErr {
                miim: self.miim,
                phy,
                page: reg.page,
                addr: reg.addr,
            });
        }

        let value = out.miim_data_rddata() as u16;
        Ok(value.into())
    }

    fn write_raw<T>(
        &self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone,
    {
        let value: u16 = value.into();
        let mut v = Self::miim_cmd(phy, reg.addr);
        v.set_miim_cmd_opr_field(0b01); // read
        v.set_miim_cmd_wrdata(value as u32);

        self.miim_idle_wait()?;
        self.vsc7448
            .write(Vsc7448::DEVCPU_GCB().MIIM(self.miim as u32).MII_CMD(), v)
    }
}
