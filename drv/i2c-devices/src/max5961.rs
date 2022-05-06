//! Driver for the MAX6961 hot-swap controller

pub enum Channel {
    Channel0 = 0,
    Channel1 = 1,
    Channel2 = 2,
    Channel3 = 3
}

pub enum Register {
    ADCCurrent(Channel),
    ADCVoltage(Channel),
    MinCurrent(Channel),
    MaxCurrent(Channel),
    Status0,
}

impl Register {
    fn groupsize(&self) -> u8 {
        match self {
            Register::ADCCurrent(_) |
            Register::ADCVoltage(_) => 4,
            Register::MinCurrent(_) |
            Register::MaxCurrent(_) => 8,
        }
    }

    fn base(&self) -> u8 {
        match self {
            Register::ADCCurrent(_) => 0x0,
            Register::ADCVoltage(_) => 0x2,
            Register::MinCurrent(_) => 0x10,
            Register::MaxCurrent(_) => 0x12,
            Register::Status0 => 0x5f,
        }
    }

    fn address(&self) -> u8 {
        match self {
            Register::ADCCurrent(channel) |
            Register::ADCVoltage(channel) |
            Register::MinCurrent(channel) |
            Register::MaxCurrent(channel) => {
                self.base() + self.groupsize() * channel.into()
            }
            _ => {
                self.base()
            }
        }
    }
}


pub struct Max5961 {
    pub device: I2cDevice,
}

impl core::fmt::Display for Max5961 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "max5961: {}", &self.device)
    }
}


    Register::UndervoltageWarningThreshold(Channel::Channel1)


