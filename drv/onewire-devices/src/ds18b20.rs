use userlib::units::*;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub enum Command {
    ConvertT = 0x44,
    WriteScratchpad = 0x4e,
    ReadScratchpad = 0xbe,
    CopyScratchpad = 0x48,
    RecallESquared = 0xb8,
    ReadPowerSupply = 0xb4,
}

#[derive(Copy, Clone)]
pub struct Ds18b20 {
    pub id: u64,
}

impl core::fmt::Display for Ds18b20 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ds18b20 0x{:012x}", (self.id >> 8) & 0xff_ffff_ffff)
    }
}

//
// Convert as per Figure 4.
//
fn convert(lsb: u8, msb: u8) -> Celsius {
    Celsius(((((msb as u16) << 8) | (lsb as u16)) as i16) as f32 / 16.0)
}

impl Ds18b20 {
    pub fn new(id: u64) -> Option<Self> {
        if drv_onewire::family(id) == Some(drv_onewire::Family::DS18B20) {
            Some(Self { id: id })
        } else {
            None
        }
    }

    pub fn convert_temperature<T>(
        &self,
        reset: impl Fn() -> Result<(), T>,
        write_byte: impl Fn(u8) -> Result<(), T>,
    ) -> Result<(), T> {
        reset()?;
        write_byte(drv_onewire::Command::MatchROM as u8)?;

        for i in 0..8 {
            write_byte(((self.id >> (i * 8)) & 0xff) as u8)?;
        }

        write_byte(Command::ConvertT as u8)?;

        Ok(())
    }

    pub fn read_temperature<T>(
        &self,
        reset: impl Fn() -> Result<(), T>,
        write_byte: impl Fn(u8) -> Result<(), T>,
        read_byte: impl Fn() -> Result<u8, T>,
    ) -> Result<Celsius, T> {
        reset()?;

        write_byte(drv_onewire::Command::MatchROM as u8)?;

        for i in 0..8 {
            write_byte(((self.id >> (i * 8)) & 0xff) as u8)?;
        }

        write_byte(Command::ReadScratchpad as u8)?;

        let lsb = read_byte()?;
        let msb = read_byte()?;

        Ok(convert(lsb, msb))
    }
}
