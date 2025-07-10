// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for AMD SB-RMI interface for AMD Turin.  This interface is both
//! AMD- and Turin-specific (and in particular, note that the number of
//! threads cannot exceed a 15-bit quantity in this processor generation).

use crate::Validate;
use bitstruct::bitstruct;
use drv_i2c_api::*;
use ringbuf::*;
use zerocopy::FromBytes;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Error {
    BadRegisterRead {
        reg: Register,
        code: ResponseCode,
    },
    BadRegisterWrite {
        reg: Register,
        code: ResponseCode,
    },
    BadRegisterBlockWrite {
        reg: [u8; 2],
        len: u8,
        code: ResponseCode,
    },
    BadRegisterBlockRead {
        reg: [u8; 2],
        len: u8,
        code: ResponseCode,
    },
    BadThreadId {
        thread: u16,
    },
    BadCpuidInput,
    BadCpuidLength {
        length: u8,
    },
    BadCpuidRead {
        code: ResponseCode,
    },
    CpuidFailed {
        code: StatusCode,
    },
    BadRdmsrLength {
        length: u8,
    },
    BadRdmsr {
        code: ResponseCode,
    },
    RdmsrFailed {
        code: StatusCode,
    },
}

type Result<T> = core::result::Result<T, Error>;

impl From<Error> for ResponseCode {
    fn from(err: Error) -> Self {
        match err {
            Error::BadRegisterRead { code, .. } => code,
            Error::BadRegisterWrite { code, .. } => code,
            Error::BadRegisterBlockWrite { code, .. } => code,
            Error::BadCpuidRead { code } => code,
            Error::BadRdmsr { code } => code,
            _ => ResponseCode::BadResponse,
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
    InvalidThread,
    UnsupportedCommand,
    CommandAborted,
    Unknown(u8),
}

bitstruct! {
    #[derive(Copy, Clone, Debug, Default)]
    pub struct SbrmiControl(u8) {
        alert_mask: bool = 0;
        tsi_soft_reset: bool = 1;
        timeout_dis: bool = 2;
        block_read_write_en: bool = 3;
        software_alert_mask: bool = 4;
        mbox_completion_software_alert_en: bool = 5;
        reserved: bool = 6;
        hardware_alert_mask: bool = 7;
    }
}

#[allow(dead_code)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    Revision,
    Control,
    Status,
    ReadSize,
    SoftwareIntr,
    ThreadNumber,
    Enabled { base: u8, offset: u8 },
    Alert { base: u8, offset: u8 },
}

#[derive(Debug)]
pub struct CpuidRead {
    thread: u16,
    func: u32,
    ecx: u8,
    read_high_half: bool,
}

impl CpuidRead {
    fn new(thread: u16, func: u32, ecx: u8, read_high_half: bool) -> CpuidRead {
        CpuidRead {
            thread,
            func,
            ecx,
            read_high_half,
        }
    }

    fn as_reg(&self) -> Result<[u8; 12]> {
        if (self.thread >> 15) != 0 {
            return Err(Error::BadThreadId {
                thread: self.thread,
            });
        }
        let thr_lo = (self.thread as u8 & 0b0111_1111) << 1;
        let thr_hi = (self.thread >> 7) as u8 & 0b1111_1111;
        let func = self.func.to_le_bytes();
        if self.ecx > 0b1111 {
            return Err(Error::BadCpuidInput);
        }
        let ecx = (self.ecx & 0b1111) << 4;
        let half: u8 = self.read_high_half.into();
        Ok([
            0x73,
            0,
            0x9,
            0x8,
            0x91,
            thr_lo,
            thr_hi,
            func[0],
            func[1],
            func[2],
            func[3],
            ecx | half,
        ])
    }
}

impl From<Register> for u8 {
    fn from(reg: Register) -> Self {
        match reg {
            Register::Revision => 0x0,
            Register::Control => 0x1,
            Register::Status => 0x2,
            Register::ReadSize => 0x3,
            Register::SoftwareIntr => 0x40,
            Register::ThreadNumber => 0x41,
            Register::Enabled { base, offset } => base + offset,
            Register::Alert { base, offset } => base + offset,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

pub const MCE_ALERT_BANK_SIZE: usize = 16;
pub const MCE_ALERT_NBANKS: usize = 2;
pub const MCE_ALERT_STATUS_SIZE: usize = MCE_ALERT_BANK_SIZE * MCE_ALERT_NBANKS;

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

pub enum MailboxCmds {
    Foo,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    CpuidCall([u8; 12]),
    CpuidResult([u8; 10]),
    RdmsrCall([u8; 11]),
    RdmsrResult([u8; 10]),
}

ringbuf!(Trace, 12, Trace::None);

impl Sbrmi {
    pub fn new(device: &I2cDevice) -> Self {
        Self { device: *device }
    }

    pub fn mailbox(&self, cmd: MailboxCmds) -> Result<()> {
        Ok(())
    }

    fn read_byte_reg(&self, reg: Register) -> Result<u8> {
        let cmd = [reg.into(), 0];
        self.device
            .read_reg(cmd)
            .map_err(|code| Error::BadRegisterRead { reg, code })
    }

    fn write_byte_reg(&self, reg: Register, val: u8) -> Result<()> {
        let cmd = [reg.into(), 0, val];
        self.device
            .write(&cmd)
            .map_err(|code| Error::BadRegisterWrite { reg, code })
    }

    fn rmw_byte_reg<F: FnOnce(u8) -> u8>(
        &self,
        reg: Register,
        thunk: F,
    ) -> Result<u8> {
        let old = self.read_byte_reg(reg)?;
        let new = thunk(old);
        self.write_byte_reg(reg, new)?;
        Ok(old)
    }

    fn read_block_reg(&self, reg: &[u8], result: &mut [u8]) -> Result<usize> {
        let saved_ctl = self.rmw_byte_reg(Register::Control, |ctl| {
            SbrmiControl(ctl)
                .with_block_read_write_en(true)
                .with_mbox_completion_software_alert_en(false)
                .0
        })?;

        let cmd = [reg[0], reg[1]];
        let len = result.len() as u8 - 1;

        self.device.write(reg).map_err(|code| {
            Error::BadRegisterBlockWrite {
                reg: cmd,
                len: len,
                code: code,
            }
        })?;

        self.write_byte_reg(Register::Control, saved_ctl)?;
        self.write_byte_reg(Register::ReadSize, len)?;
        self.write_byte_reg(Register::SoftwareIntr, 1)?;
        loop {
            let intr = self.read_byte_reg(Register::SoftwareIntr)?;
            if intr == 0 {
                break;
            }
        }
        let saved_ctl = self.rmw_byte_reg(Register::Control, |ctl| {
            SbrmiControl(ctl)
                .with_block_read_write_en(true)
                .with_mbox_completion_software_alert_en(false)
                .0
        })?;
        self.device.read_reg_into(cmd, result).map_err(|code| {
            Error::BadRegisterBlockRead {
                reg: cmd,
                len: len,
                code: code,
            }
        })?;
        self.write_byte_reg(Register::Control, saved_ctl)?;
        self.write_byte_reg(Register::ReadSize, 1)?;
        Ok(result[0] as usize)
    }

    #[allow(dead_code)]
    fn write_block_reg(&self, reg: &[u8]) -> Result<()> {
        let saved_ctl = self.rmw_byte_reg(Register::Control, |ctl| {
            SbrmiControl(ctl)
                .with_block_read_write_en(true)
                .with_mbox_completion_software_alert_en(false)
                .0
        })?;

        let cmd = [reg[0], reg[1]];
        let len = reg[2];

        self.device.write(reg).map_err(|code| {
            Error::BadRegisterBlockWrite {
                reg: cmd,
                len: len,
                code: code,
            }
        })?;

        self.write_byte_reg(Register::Control, saved_ctl)
    }

    pub fn nthreads(&self) -> Result<u8> {
        self.read_byte_reg(Register::ThreadNumber)
    }

    pub fn enabled(&self) -> Result<[u8; 32]> {
        // The enabled bits are found in several discontiguous, and
        // differently sized, banks across the register space.  However,
        // threads are contiguous within the banks.
        let mut rval = [0u8; 32];
        let n = [(0x4, 2), (0x8, 6), (0x43, 8), (0x91, 8), (0xD8, 8)]
            .into_iter()
            .try_fold(0, |index, (base, len)| {
                for offset in 0..len {
                    let k = index + offset;
                    let offset = offset.try_into().unwrap();
                    let reg = Register::Enabled { base, offset };
                    rval[k] = self.read_byte_reg(reg)?;
                }
                Ok(index + len)
            })?;
        assert_eq!(usize::from(n), rval.len());

        Ok(rval)
    }

    // MCE Alert Status registers are organized into two discontiguous
    // banks in the SB-RMI register space.  Each bank has 16 8-bit,
    // registers, for a total of 32 byte-sized registers.  We refer to
    // registers in the concatenation of the two banks as "instances".
    // So instances 0..=15 are in the first bank, and instances 16..=31
    // are in the second bank.
    //
    // The mapping between thread number and register instance is
    // abstruce:
    //
    // * Status for threads 0..=63 is in the low 4 bits of the 8-bit
    //   registers in the first bank (instances [0..=15])
    // * Status for threads 64..=127 occupy the low 4 bits of registers
    //   of the second bank (instances [16..=31])
    // * Threads 128..=191 status in the high 4 bits of instances [0..=16]
    // * Threads 192..=255 status in the high 4 bits of instances [16..=31]
    //
    // In all cases, the bit index for each thread in the corresponding
    // register is derived from the thread number divided by 16.
    // Graphically, the mapping is defined as:
    //
    // REGISTER  DESCRIPTION
    // 0x10      MceStat[7:0] = Threads[176,160,144,128, 48,32,16,0]
    // 0x11      MceStat[7:0] = Threads[177,161,145,129, 49,33,17,1]
    // 0x12      MceStat[7:0] = Threads[178,162,146,130, 50,34,18,2]
    // 0x13      MceStat[7:0] = Threads[179,163,147,131, 51,35,19,3]
    // 0x14      MceStat[7:0] = Threads[180,164,148,132, 52,36,20,4]
    // 0x15      MceStat[7:0] = Threads[181,165,149,133, 53,37,21,5]
    // 0x16      MceStat[7:0] = Threads[182,166,150,134, 54,38,22,6]
    // 0x17      MceStat[7:0] = Threads[183,167,151,135, 55,39,23,7]
    // 0x18      MceStat[7:0] = Threads[184,168,152,136, 56,40,24,8]
    // 0x19      MceStat[7:0] = Threads[185,169,153,137, 57,41,25,9]
    // 0x1A      MceStat[7:0] = Threads[186,170,154,138, 58,42,26,10]
    // 0x1B      MceStat[7:0] = Threads[187,171,155,139, 59,43,27,11]
    // 0x1C      MceStat[7:0] = Threads[188,172,156,140, 60,44,28,12]
    // 0x1D      MceStat[7:0] = Threads[189,173,157,141, 61,45,29,13]
    // 0x1E      MceStat[7:0] = Threads[190,174,158,142, 62,46,30,14]
    // 0x1F      MceStat[7:0] = Threads[191,175,159,143, 63,47,31,15]
    //
    // 0x50      MceStat[7:0] = Threads[240,224,208,192, 112,96,80,64]
    // 0x51      MceStat[7:0] = Threads[241,225,209,193, 113,97,81,65]
    // 0x52      MceStat[7:0] = Threads[242,226,210,194, 114,98,82,66]
    // 0x53      MceStat[7:0] = Threads[243,227,211,195, 115,99,83,67]
    // 0x54      MceStat[7:0] = Threads[244,228,212,196, 116,100,84,68]
    // 0x55      MceStat[7:0] = Threads[245,229,213,197, 117,101,85,69]
    // 0x56      MceStat[7:0] = Threads[246,230,214,198, 118,102,86,70]
    // 0x57      MceStat[7:0] = Threads[247,231,215,199, 119,103,87,71]
    // 0x58      MceStat[7:0] = Threads[248,232,216,200, 120,104,88,72]
    // 0x59      MceStat[7:0] = Threads[249,233,217,201, 121,105,89,73]
    // 0x5A      MceStat[7:0] = Threads[250,234,218,202, 122,106,90,74]
    // 0x5B      MceStat[7:0] = Threads[251,235,219,203, 123,107,91,75]
    // 0x5C      MceStat[7:0] = Threads[252,236,220,204, 124,108,92,76]
    // 0x5D      MceStat[7:0] = Threads[253,237,221,205, 125,109,93,77]
    // 0x5E      MceStat[7:0] = Threads[254,238,222,206, 126,110,94,78]
    // 0x5F      MceStat[7:0] = Threads[255,239,223,207, 127,111,95,79]
    //
    // Observe the space of thread to register can be logically divided
    // into quadrants: threads 0..=63 in quadrant 0 (upper right),
    // 64..=127 in quadrant 1 (lower right), 128..=191 in quadrant 2
    // (upper left), and 192..=255 in quadrant 3 (lower left).  Note that
    // each bank thus covers two quadrants.
    //
    // Given this, we can derive the mapping of thread number to register
    // index and bit number within a register as follows:
    //
    // * Quadrant number is thread no / quadrant size
    // * Register index within a quadrant is thread no % 16
    // * Bit number within the quadrant nibble of a register
    //   is (thread no / 16) % 4
    // * Bit number within the register as a whole is bit no within
    //   the quadrant + 4 * (quadrant no / 2).  This can be written as
    //   (thread no / 16) % 4 + 4 * (thread no / 128).
    // * Register base index for each quadrant is 16 * (quadrant no % 2)
    //
    // We fetch all MCE alert status registers sequentially, and accumulate
    // these into a bit vector indexed by thread number, so we must convert
    // from the bit offset within a given register to the thread number.
    // Fortunately we know the bank number, register number within the
    // bank, and bit number, so we can these and the arithmetic described
    //  above to calculate the thread number.
    //
    // The quadrant number is a function of both the bank and the bit
    // number within the register.  If the bit is in the high nibble,
    // we add an offset of 2 to the bank number, which could be written
    // as bank + (bit / 4) * 2; we can write this using only and and
    // a shift by extracting the third bit from the byte and yielding
    // either 4 or 0, and shifting the result right by 1, thus dividing
    // it by two, giving either 2 or 0.  we
    // get the quadrant number, origin 0.
    //
    // Given that, we can find the thread number base for the quadrant
    // by multiplying by the quadrant size.  We calculate the thread
    // offset from the specific register by taking the bit within the
    // quadrant-specific nibble in the register and multiplying it by
    // the bank size.  Finally, we add the register index to get the
    // thread number.

    fn reg2thread(bank: usize, bank_reg: usize, bit: usize) -> usize {
        let quadrant_offset = (bit & 0b0100) >> 1;
        let quadrant = bank + quadrant_offset as usize;
        const QUADRANT_SIZE: usize = 64;
        let quadrant_base = quadrant * QUADRANT_SIZE;
        let nibble_bit = bit % 4;
        let bit_offset = nibble_bit * MCE_ALERT_BANK_SIZE;
        let threadno = quadrant_base + bit_offset + bank_reg;
        threadno
    }

    pub fn alert(&self) -> Result<[u8; MCE_ALERT_STATUS_SIZE]> {
        const MCE_ALERT_BANK_BASE_ADDRS: [u8; MCE_ALERT_NBANKS] = [0x10, 0x50];
        let mut rval = [0u8; MCE_ALERT_STATUS_SIZE];
        for (bank, base) in MCE_ALERT_BANK_BASE_ADDRS.into_iter().enumerate() {
            for bank_reg in 0..MCE_ALERT_BANK_SIZE {
                let offset = bank_reg as u8;
                let mce_alert_status =
                    self.read_byte_reg(Register::Alert { base, offset })?;
                for bit in 0..8 {
                    let mask = 1 << bit;
                    if mce_alert_status & mask == 0 {
                        continue;
                    }
                    let threadno = Self::reg2thread(bank, bank_reg, bit);
                    let index = threadno / MCE_ALERT_STATUS_SIZE;
                    let offset = threadno % 8;
                    rval[index] |= 1 << offset;
                }
            }
        }

        Ok(rval)
    }

    fn result_to_u32(result: &[u8]) -> u32 {
        u32::from_le_bytes(result[..4].try_into().unwrap())
    }

    pub fn cpuid(&self, thread: u8, eax: u32, ecx: u32) -> Result<CpuidResult> {
        // We need to do two reads to get the full set of registers returned.
        let mut rval: CpuidResult = Default::default();
        for hihalf in [false, true] {
            let mut result = [0u8; 10];
            let req = CpuidRead::new(thread as u16, eax, ecx as u8, hihalf);
            let reg = req.as_reg()?;
            ringbuf_entry!(Trace::CpuidCall(reg));
            self.read_block_reg(&reg, &mut result)?;
            ringbuf_entry!(Trace::CpuidResult(result));
            if result[0] != 9 {
                return Err(Error::BadCpuidLength { length: result[0] });
            }
            let code = StatusCode::from(result[1]);
            if code != StatusCode::Success {
                return Err(Error::CpuidFailed { code });
            }
            if !hihalf {
                rval.eax = Self::result_to_u32(&result[2..=5]);
                rval.ebx = Self::result_to_u32(&result[6..=9]);
            } else {
                rval.ecx = Self::result_to_u32(&result[2..=5]);
                rval.edx = Self::result_to_u32(&result[6..=9]);
            }
        }
        Ok(rval)
    }

    pub fn rdmsr<V: FromBytes>(&self, thread: u8, msr: u32) -> Result<V> {
        let size = core::mem::size_of::<V>() as u8;
        let msr = msr.to_le_bytes();

        let request: [u8; 11] = [
            0x73, // Read CPUID/Read Register Command Format
            0,
            0x7,                         // Payload size
            size,                        // Desired return size
            0x86,                        // Read register command
            (thread & 0b0111_1111) << 1, // Thread ID
            (thread >> 7) & 0b1111_1111,
            msr[0], // address[7:0]
            msr[1], // address[15:8]
            msr[2], // address[23:16]
            msr[3], // address[31:24]
        ];

        ringbuf_entry!(Trace::RdmsrCall(request));

        let mut result = [0u8; 10];
        if let Err(code) = self.device.read_reg_into(request, &mut result) {
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

impl Validate<Error> for Sbrmi {
    fn validate(device: &I2cDevice) -> Result<bool> {
        let sbrmi = Sbrmi::new(device);
        let rev = sbrmi.read_byte_reg(Register::Revision)?;
        Ok(rev == 0x21)
    }
}
