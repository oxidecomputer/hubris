// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for AMD SB-RMI interface

use crate::Validate;
use drv_i2c_api::*;
use ringbuf::*;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    Revision = 0x0,
    Control = 0x01,
    Status = 0x02,
    ReadSize = 0x03,
    ThreadNumber = 0x41,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum StatusCode {
    Success,
    CommandTimeout,
    WarmReset,
    UnknownCommandFormat,
    InvalidReadLength,
    ExcessiveData,
    InvalidThread,
    UnsupportedCommand,
    CommandAborted,
    Unknown(u8),
}

#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Error {
    BadRegisterRead { reg: Register, code: ResponseCode },
    BadThreadId,
    BadCpuidInput,
    BadCpuidLength { length: u8 },
    BadCpuidRead { code: ResponseCode },
    CpuidFailed { code: StatusCode },
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRegisterRead { code, .. } => code,
            Error::BadCpuidRead { code } => code,
            _ => ResponseCode::BadResponse,
        }
    }
}

pub struct Sbrmi {
    device: I2cDevice,
}

impl From<u8> for StatusCode {
    fn from(code: u8) -> Self {
        match code {
            0 => StatusCode::Success,
            0x11 => StatusCode::CommandTimeout,
            0x22 => StatusCode::WarmReset,
            0x40 => StatusCode::UnknownCommandFormat,
            0x41 => StatusCode::InvalidReadLength,
            0x42 => StatusCode::ExcessiveData,
            0x44 => StatusCode::InvalidThread,
            0x45 => StatusCode::UnsupportedCommand,
            0x81 => StatusCode::CommandAborted,
            _ => StatusCode::Unknown(code),
        }
    }
}

impl core::fmt::Display for Sbrmi {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "sbrmi: {}", &self.device)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    CpuidCall([u8; 10]),
    CpuidResult([u8; 10]),
}

ringbuf!(Trace, 12, Trace::None);

impl Sbrmi {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    fn read_reg(&self, reg: Register) -> Result<u8, Error> {
        match self.device.read_reg::<u8, u8>(reg as u8) {
            Ok(buf) => Ok(buf),
            Err(code) => Err(Error::BadRegisterRead { reg, code }),
        }
    }

    fn result_to_u32(result: &[u8]) -> u32 {
        u32::from_le_bytes(result[..4].try_into().unwrap())
    }

    pub fn cpuid(
        &self,
        thread: u8,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuidResult, Error>   {
        let eax = eax.to_le_bytes();
        let mut rval = CpuidResult { ..Default::default() };

        if (thread >> 7) != 0 {
            return Err(Error::BadThreadId);
        }

        if (ecx >> 4) != 0 {
            return Err(Error::BadCpuidInput);
        }

        //
        // We need to do two reads to get the full set of registers returned.
        //
        for regset in 0..=1 {
            let mut result = [0u8; 10];

            let call: [u8; 10] = [
                0x73,               // Read CPUID/Read Register Command Format
                0x8,                // Payload size
                0x8,                // Desired return size
                0x91,               // CPUID command
                thread << 1,        // Thread ID
                eax[0],             // EAX[7:0]
                eax[1],             // EAX[15:8]
                eax[2],             // EAX[23:16]
                eax[3],             // EAX[31:24]
                (((ecx & 0xf) << 4) | regset) as u8, // ECX[3:0] + reg set
            ];

            ringbuf_entry!(Trace::CpuidCall(call));

            if let Err(code) = self.device.read_reg_into(call, &mut result) {
                return Err(Error::BadCpuidRead { code });
            }

            ringbuf_entry!(Trace::CpuidResult(result));

            if result[0] == 0 || result[0] > (result.len() - 1) as u8 {
                return Err(Error::BadCpuidLength { length: result[0] });
            }

            let code = StatusCode::from(result[1]);

            if code != StatusCode::Success {
                return Err(Error::CpuidFailed { code })
            }

            if regset == 0 {
                rval.eax = Self::result_to_u32(&result[2..]);
                rval.ebx = Self::result_to_u32(&result[6..]);
            } else {
                rval.ecx = Self::result_to_u32(&result[2..]);
                rval.edx = Self::result_to_u32(&result[6..]);
            }
        }

        Ok(rval)
    }
}

impl Validate<Error> for Sbrmi {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let sbrmi = Sbrmi::new(device);
        let rev = sbrmi.read_reg(Register::Revision)?;

        Ok(rev == 0x10)
    }
}
