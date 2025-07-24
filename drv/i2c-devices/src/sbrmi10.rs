// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for AMD SB-RMI interface for AMD Milan.  This interface is both
//! AMD- and Milan-specific (and in particular, note that the number of
//! threads cannot exceed a 7-bit quantity in this processor generation).

use crate::Validate;
use drv_i2c_api::*;
use ringbuf::*;
use zerocopy::FromBytes;

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    Revision,
    Control,
    Status,
    ReadSize,
    ThreadNumber,
    Enabled { base: u8, offset: u8 },
    Alert { base: u8, offset: u8 },
}

impl From<Register> for u8 {
    fn from(reg: Register) -> Self {
        match reg {
            Register::Revision => 0x0,
            Register::Control => 0x1,
            Register::Status => 0x2,
            Register::ReadSize => 0x3,
            Register::ThreadNumber => 0x41,
            Register::Enabled { base, offset } => base + offset,
            Register::Alert { base, offset } => base + offset,
        }
    }
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
    BadRdmsrLength { length: u8 },
    BadRdmsr { code: ResponseCode },
    RdmsrFailed { code: StatusCode },
}

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRegisterRead { code, .. } => code,
            Error::BadCpuidRead { code } => code,
            Error::BadRdmsr { code } => code,
            _ => ResponseCode::BadResponse,
        }
    }
}

pub struct Sbrmi10 {
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

impl core::fmt::Display for Sbrmi10 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "sbrmi: {}", &self.device)
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    CpuidCall([u8; 10]),
    CpuidResult([u8; 10]),
    RdmsrCall([u8; 9]),
    RdmsrResult([u8; 10]),
}

ringbuf!(Trace, 12, Trace::None);

impl Sbrmi10 {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    fn read_reg(&self, reg: Register) -> Result<u8, Error> {
        self.device
            .read_reg::<u8, u8>(reg.into())
            .map_err(|code| Error::BadRegisterRead { reg, code })
    }

    pub fn nthreads(&self) -> Result<u8, Error> {
        self.read_reg(Register::ThreadNumber)
    }

    pub fn enabled(&self) -> Result<[u8; 16], Error> {
        //
        // The enabled bits are found in three different banks across the
        // register space.  The banks are asymmetric (which is slightly
        // annoying), but -- unlike the alert bits -- at least the enabled
        // bits are stored contiguously by thread...
        //
        let mut rval = [0u8; 16];
        let mut roffs = 0;

        for (base, banksize) in &[(0x4, 2), (0x8, 6), (0x43, 8)] {
            for offs in 0..*banksize {
                rval[roffs + offs] = self.read_reg(Register::Enabled {
                    base: *base,
                    offset: offs as u8,
                })?;
            }

            roffs += banksize;
        }

        Ok(rval)
    }

    pub fn alert(&self) -> Result<[u8; 16], Error> {
        //
        // Have you ever played 52-bit Pickup?  Unlike the enabled bits, the
        // alert bits are smeared and interleaved across the register space.
        // Each byte has 4 bits, with each bit representing the alert status
        // of a thread; here is the mapping of registers to threads:
        //
        //   REGISTER  DESCRIPTION
        //   0x10      MceStat[3:0] = Threads[48,32,16,0]
        //   0x11      MceStat[3:0] = Threads[49,33,17,1]
        //   0x12      MceStat[3:0] = Threads[50,34,18,2]
        //   0x13      MceStat[3:0] = Threads[51,35,19,3]
        //   0x14      MceStat[3:0] = Threads[52,36,20,4]
        //   0x15      MceStat[3:0] = Threads[53,37,21,5]
        //   0x16      MceStat[3:0] = Threads[54,38,22,6]
        //   0x17      MceStat[3:0] = Threads[55,39,23,7]
        //   0x18      MceStat[3:0] = Threads[56,40,24,8]
        //   0x19      MceStat[3:0] = Threads[57,41,25,9]
        //   0x1A      MceStat[3:0] = Threads[58,42,26,10]
        //   0x1B      MceStat[3:0] = Threads[59,43,27,11]
        //   0x1C      MceStat[3:0] = Threads[60,44,28,12]
        //   0x1D      MceStat[3:0] = Threads[61,45,29,13]
        //   0x1E      MceStat[3:0] = Threads[62,46,30,14]
        //   0x1F      MceStat[3:0] = Threads[63,47,31,15]
        //   0x50      MceStat[3:0] = Threads[112,96,80,64]
        //   0x51      MceStat[3:0] = Threads[113,97,81,65]
        //   0x52      MceStat[3:0] = Threads[114,98,82,66]
        //   0x53      MceStat[3:0] = Threads[115,99,83,67]
        //   0x54      MceStat[3:0] = Threads[116,100,84,68]
        //   0x55      MceStat[3:0] = Threads[117,101,85,69]
        //   0x56      MceStat[3:0] = Threads[118,102,86,70]
        //   0x57      MceStat[3:0] = Threads[119,103,87,71]
        //   0x58      MceStat[3:0] = Threads[120,104,88,72]
        //   0x59      MceStat[3:0] = Threads[121,105,89,73]
        //   0x5A      MceStat[3:0] = Threads[122,106,90,74]
        //   0x5B      MceStat[3:0] = Threads[123,107,91,75]
        //   0x5C      MceStat[3:0] = Threads[124,108,92,76]
        //   0x5D      MceStat[3:0] = Threads[125,109,93,77]
        //   0x5E      MceStat[3:0] = Threads[126,110,94,78]
        //   0x5F      MceStat[3:0] = Threads[127,111,95,79]
        //
        let mut rval = [0u8; 16];
        let banksize = 16usize;

        for (bank, base) in [0x10, 0x50].iter().enumerate() {
            for offs in 0..banksize {
                let alert = self.read_reg(Register::Alert {
                    base: *base,
                    offset: offs as u8,
                })?;

                let which = (offs >> 3) & 1;

                for bit in 0..4 {
                    if (alert & (1 << bit)) != 0 {
                        let byte = (bank * 8) + (bit << 1) + which;
                        rval[byte] |= 1 << (offs & 0x7);
                    }
                }
            }
        }

        Ok(rval)
    }

    fn result_to_u32(result: &[u8]) -> u32 {
        u32::from_le_bytes(result[..4].try_into().unwrap())
    }

    pub fn cpuid(
        &self,
        thread: u8,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuidResult, Error> {
        let eax = eax.to_le_bytes();
        let mut rval: CpuidResult = Default::default();

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
                0x73,        // Read CPUID/Read Register Command Format
                0x8,         // Payload size
                0x8,         // Desired return size
                0x91,        // CPUID command
                thread << 1, // Thread ID
                eax[0],      // EAX[7:0]
                eax[1],      // EAX[15:8]
                eax[2],      // EAX[23:16]
                eax[3],      // EAX[31:24]
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
                return Err(Error::CpuidFailed { code });
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

    pub fn rdmsr<V: FromBytes>(
        &self,
        thread: u8,
        msr: u32,
    ) -> Result<V, Error> {
        let size = core::mem::size_of::<V>() as u8;
        let msr = msr.to_le_bytes();
        let mut result = [0u8; 10];

        let call: [u8; 9] = [
            0x73,        // Read CPUID/Read Register Command Format
            0x7,         // Payload size
            size,        // Desired return size
            0x86,        // Read register command
            thread << 1, // Thread ID
            msr[0],      // address[7:0]
            msr[1],      // address[15:8]
            msr[2],      // address[23:16]
            msr[3],      // address[31:24]
        ];

        ringbuf_entry!(Trace::RdmsrCall(call));

        if let Err(code) = self.device.read_reg_into(call, &mut result) {
            return Err(Error::BadRdmsr { code });
        }

        ringbuf_entry!(Trace::RdmsrResult(result));

        if result[0] == 0 || result[0] > size + 1 {
            return Err(Error::BadRdmsrLength { length: result[0] });
        }

        let code = StatusCode::from(result[1]);

        if code != StatusCode::Success {
            return Err(Error::RdmsrFailed { code });
        }

        Ok(<V>::read_from_bytes(&result[2..2 + size as usize]).unwrap())
    }
}

impl Validate<Error> for Sbrmi10 {
    fn validate(device: &I2cDevice) -> Result<bool, Error> {
        let sbrmi = Sbrmi10::new(device);
        let rev = sbrmi.read_reg(Register::Revision)?;

        Ok(rev == 0x10)
    }
}
