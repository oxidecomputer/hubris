/// Trait implementing communication with an ethernet PHY.
pub trait PhyRw {
    /// Reads a register from the PHY without changing the page.  This should
    /// never be called directly, because the page could be incorrect, but
    /// it's a required building block for `read`
    fn read_raw<T: From<u16>>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
    ) -> Result<T, VscError>;

    /// Writes a register to the PHY without changing the page.  This should
    /// never be called directly, because the page could be incorrect, but
    /// it's a required building block for `read` and `write`
    fn write_raw<T>(
        &mut self,
        phy: u8,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        u16: From<T>,
        T: From<u16> + Clone;
}

/// Handle for interacting with a particular PHY port
pub struct Phy<'a, P> {
    pub port: u8,
    pub rw: &'a mut P,
}

impl<P: PhyRw> Phy<'_, P> {
    pub fn read<T>(&mut self, reg: PhyRegisterAddress<T>) -> Result<T, VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.rw.write_raw::<phy::standard::PAGE>(
            self.port,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        self.rw.read_raw(self.port, reg)
    }

    pub fn write<T>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        value: T,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
    {
        self.rw.write_raw::<phy::standard::PAGE>(
            self.port,
            phy::STANDARD::PAGE(),
            reg.page.into(),
        )?;
        self.rw.write_raw(self.port, reg, value)
    }

    pub fn write_with<T, F>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(&mut T),
    {
        let mut data = 0.into();
        f(&mut data);
        self.write(reg, data)
    }

    /// Performs a read-modify-write operation on a PHY register connected
    /// to the VSC7448 via MIIM.
    pub fn modify<T, F>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(&mut T),
    {
        let mut data = self.read(reg)?;
        f(&mut data);
        self.write(reg, data)
    }

    pub fn wait_timeout<T, F>(
        &mut self,
        reg: PhyRegisterAddress<T>,
        f: F,
    ) -> Result<(), VscError>
    where
        T: From<u16> + Clone,
        u16: From<T>,
        F: Fn(T) -> bool,
    {
        for _ in 0..32 {
            let r = self.read(reg)?;
            if f(r) {
                return Ok(());
            }
            sleep_for(1)
        }
        Err(VscError::PhyInitTimeout)
    }
}
