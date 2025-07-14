// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Driver for AMD SB-RMI interface for AMD Turin.  This interface is both
//! AMD- and Turin-specific (and in particular, note that the number of
//! threads cannot exceed a 15-bit quantity in this processor generation).

use core::marker::PhantomData;

use crate::Validate;
use apml_rs::SbRmi21MailboxCmd as MailboxCmd;
use apml_rs::SpdSbBusOvrdOp;
use bitstruct::bitstruct;
use drv_i2c_api::I2cDevice;
use drv_sbrmi21_api::SbRmi21Error as Error;
use drv_sbrmi21_api::SbRmi21MailboxErrorCode as MailboxErrorCode;
use drv_sbrmi21_api::StatusCode;
use ringbuf::{ringbuf, ringbuf_entry};
use zerocopy::FromBytes;

type Result<T> = core::result::Result<T, Error>;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Register {
    Revision,
    Control,
    Status,
    ReadSize,
    SoftwareIntr,
    ThreadNumberLo,
    ThreadNumberHi,
    Enabled { base: u8, offset: u8 },
    Alert { base: u8, offset: u8 },
    // Mailbox interface: SP -> SP5
    OutboundMsg0,
    OutboundMsg1,
    OutboundMsg2,
    OutboundMsg3,
    OutboundMsg4,
    OutboundMsg5,
    OutboundMsg6,
    OutboundMsg7,
    // Mailbox interface: SP5 -> SP
    InboundMsg0,
    InboundMsg1,
    InboundMsg2,
    InboundMsg3,
    InboundMsg4,
    InboundMsg5,
    InboundMsg6,
    InboundMsg7,
}

bitstruct! {
    #[derive(Copy, Clone, Debug, Default)]
    pub struct SbRmiControl(u8) {
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

bitstruct! {
    #[derive(Copy, Clone, Debug, Default)]
    pub struct SbRmiStatus(u8) {
        alert_status: bool = 0;
        software_alert_status: bool = 1;    // Write 1 to clear.
        reserved: u8 = 2..=5;
        mp0_alert_status: bool = 6;
        hardware_alert_status: bool = 7;
    }
}

#[derive(Debug)]
pub struct CpuidRead {
    thread: u32,
    func: u32,
    ecx: u8,
    read_high_half: bool,
}

impl CpuidRead {
    fn new(thread: u32, func: u32, ecx: u8, read_high_half: bool) -> CpuidRead {
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
    fn from(reg: Register) -> u8 {
        match reg {
            Register::Revision => 0x0,
            Register::Control => 0x1,
            Register::Status => 0x2,
            Register::ReadSize => 0x3,
            Register::SoftwareIntr => 0x40,
            Register::ThreadNumberLo => 0x4e,
            Register::ThreadNumberHi => 0x4f,
            Register::Enabled { base, offset } => base + offset,
            Register::Alert { base, offset } => base + offset,
            Register::OutboundMsg0 => 0x30,
            Register::OutboundMsg1 => 0x31,
            Register::OutboundMsg2 => 0x32,
            Register::OutboundMsg3 => 0x33,
            Register::OutboundMsg4 => 0x34,
            Register::OutboundMsg5 => 0x35,
            Register::OutboundMsg6 => 0x36,
            Register::OutboundMsg7 => 0x37,
            Register::InboundMsg0 => 0x38,
            Register::InboundMsg1 => 0x39,
            Register::InboundMsg2 => 0x3a,
            Register::InboundMsg3 => 0x3b,
            Register::InboundMsg4 => 0x3c,
            Register::InboundMsg5 => 0x3d,
            Register::InboundMsg6 => 0x3e,
            Register::InboundMsg7 => 0x3f,
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

struct DecodedCommand {
    cmd: u8,
    argument: Option<u32>,
    has_result: bool,
}

impl From<MailboxCmd> for DecodedCommand {
    fn from(mbox_cmd: MailboxCmd) -> Self {
        let (cmd, argument, has_result) = match mbox_cmd {
            MailboxCmd::ReadPkgPwrConsumption => (0x01, None, true),
            MailboxCmd::WritePkgPwrLim(lim) => {
                (0x02, Some(lim.milliwatts()), false)
            }
            MailboxCmd::ReadPkgPwrLim => (0x03, None, true),
            MailboxCmd::ReadMaxPkgPwrLim => (0x04, None, true),
            MailboxCmd::ReadTdp => (0x05, None, true),
            MailboxCmd::ReadMaxcTdp => (0x06, None, true),
            MailboxCmd::ReadMincTdp => (0x07, None, true),
            MailboxCmd::ReadBiosBoostFMax(coreid) => {
                (0x08, Some(coreid.into()), true)
            }
            MailboxCmd::ReadApmlBoostLim(coreid) => {
                (0x09, Some(coreid.into()), true)
            }
            MailboxCmd::WriteApmlBoostLim(coreid, freq_lim) => {
                (0x0a, Some(u32::from(coreid) | u32::from(freq_lim)), false)
            }
            MailboxCmd::WriteApmlBoostLimAllCores(freq_lim) => {
                (0x0b, Some(u32::from(freq_lim)), false)
            }
            MailboxCmd::ReadDramThrottle => (0x0c, None, true),
            MailboxCmd::WriteDramThrottle(dram_throttle) => {
                (0x0d, Some(u32::from(dram_throttle)), false)
            }
            MailboxCmd::ReadProchotStatus => (0x0e, None, true),
            MailboxCmd::ReadProchotResidency => (0x0f, None, true),
            MailboxCmd::ReadIodBistResult => (0x13, None, true),
            MailboxCmd::ReadCcdBistResult(ccd_inst) => {
                (0x14, Some(u32::from(ccd_inst)), true)
            }
            MailboxCmd::ReadCcxBistResult(ccx_inst) => {
                (0x15, Some(u32::from(ccx_inst)), true)
            }
            MailboxCmd::ReadCclkFreqLim => (0x16, None, true),
            MailboxCmd::ReadSockC0Residency => (0x17, None, true),
            MailboxCmd::GetMaxDdrBwAndUtil => (0x18, None, true),
            MailboxCmd::GetMp1FirmwareVers => (0x1c, None, true),
            MailboxCmd::InitFuseSample(counter) => {
                (0x1d, Some(u32::from(counter) & 0xF), false)
            }
            MailboxCmd::ReadFuseSettings => (0x1e, None, true),
            MailboxCmd::ReadPpinFuse(ppin_fuse_bank) => {
                (0x1f, Some(u32::from(ppin_fuse_bank)), true)
            }
            MailboxCmd::ReadPostCode(postcode_offset) => {
                (0x20, Some(u32::from(postcode_offset) & 0b1111), true)
            }
            MailboxCmd::ReadRtc(offset) => {
                (0x21, Some(u32::from(offset) * 4), true)
            }
            MailboxCmd::ReadPubDieSerNo(dword_offset, category, chiplet_no) => {
                let offset = u32::from(dword_offset) << 16;
                let chipno = u32::from(chiplet_no) << 8;
                let cat = category as u32;
                (0x22, Some(offset | chipno | cat), true)
            }
            MailboxCmd::SpdSidebandBusClearOverride(op, ovrride) => {
                let opbit = match op {
                    SpdSbBusOvrdOp::Get => 1 << 31,
                    SpdSbBusOvrdOp::Set => 0,
                };
                let ovrd = if opbit == 0 { u32::from(ovrride) } else { 0 };
                (0x23, Some(opbit | ovrd), true)
            }
            MailboxCmd::WriteFastPptLim(fppt) => {
                (0x31, Some(fppt.milliwatts()), false)
            }
            MailboxCmd::WriteThermCtlLim(temperature) => {
                (0x34, Some(temperature.degrees_celsius()), false)
            }
            MailboxCmd::WriteVrmVddCurrentLim(limit) => {
                (0x35, Some(limit.milliamps()), false)
            }
            MailboxCmd::WriteVrmVddMaxCurrentLim(limit) => {
                (0x36, Some(limit.milliamps()), false)
            }
            MailboxCmd::BmcReportDimmPwrConsumption(power, rate, dimm_addr) => {
                let power = power.milliwatts() << 17;
                let rate = (rate.millisec() as u32) << 8;
                (0x40, Some(power | rate | u32::from(dimm_addr)), false)
            }
            MailboxCmd::BmcReportDimmThermSensor(
                temperature,
                rate,
                dimm_addr,
            ) => {
                let degc = temperature.degrees_celsius() << 21;
                let rate = rate.millisec() << 8;
                (0x41, Some(degc | rate | u32::from(dimm_addr)), false)
            }
            MailboxCmd::BmcRasPcieConfigAccess(
                seg,
                dwordno,
                bus,
                dev,
                func,
            ) => {
                let seg = u32::from(seg) << 28;
                let offset = dwordno.offset();
                let bus = u32::from(bus) << 8;
                let dev = u32::from(dev) << 3;
                let func = u32::from(func);
                (0x42, Some(seg | offset | bus | dev | func), true)
            }
            MailboxCmd::BmcRasMcaValidityCheck => (0x43, None, true),
            MailboxCmd::BmcRasMcaMsrDump(bank, dwordno) => {
                let bank = u32::from(bank) << 16;
                let offset = dwordno.offset();
                (0x44, Some(bank | offset), true)
            }
            MailboxCmd::BmcRasFchResetReason(reg) => {
                (0x45, Some(u32::from(reg)), true)
            }
            MailboxCmd::GetDimmTempRangeAndRefreshRate(dimm_addr) => {
                (0x46, Some(u32::from(dimm_addr)), true)
            }
            MailboxCmd::GetDimmPwrConsumption(dimm_addr) => {
                (0x47, Some(u32::from(dimm_addr)), true)
            }
            MailboxCmd::GetDimmThermSensor(dimm_addr) => {
                (0x48, Some(u32::from(dimm_addr)), true)
            }
            MailboxCmd::PwrCurrentActiveFreqLimSocket => (0x49, None, true),
            MailboxCmd::PwrCurrentActiveFreqLimCore(coreid) => {
                (0x4a, Some(u32::from(coreid)), true)
            }
            MailboxCmd::PwrSviTelemetryAllRails => (0x4b, None, true),
            MailboxCmd::GetSockFreqRange => (0x4c, None, true),
        };
        DecodedCommand {
            cmd,
            argument,
            has_result,
        }
    }
}

pub const MCE_ALERT_BANK_SIZE: usize = 16;
pub const MCE_ALERT_NBANKS: usize = 2;
pub const MCE_ALERT_STATUS_SIZE: usize = MCE_ALERT_BANK_SIZE * MCE_ALERT_NBANKS;

pub trait SbRmiMessageProto {}

#[derive(Debug)]
pub struct BlockProto;
impl SbRmiMessageProto for BlockProto {}

#[derive(Debug)]
pub struct ByteProto;
impl SbRmiMessageProto for ByteProto {}

pub struct SbRmi<T: SbRmiMessageProto> {
    proto: PhantomData<T>,
    device: I2cDevice,
}

impl<T: SbRmiMessageProto> SbRmi<T> {
    pub fn new(device: I2cDevice) -> Result<SbRmi<ByteProto>> {
        let sbrmi = SbRmi {
            proto: PhantomData,
            device,
        };
        sbrmi.set_block_xfer_en(false)?;
        Ok(sbrmi)
    }

    fn read_byte_reg(&self, reg: Register) -> Result<u8> {
        let reg = reg.into();
        let cmd = [reg, 0];
        self.device
            .read_reg(cmd)
            .map_err(|code| Error::BadRegisterRead { reg, code })
    }

    fn write_byte_reg(&self, reg: Register, val: u8) -> Result<()> {
        let reg = reg.into();
        let cmd = [reg, 0, val];
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

    fn set_block_xfer_en(&self, value: bool) -> Result<u8> {
        self.rmw_byte_reg(Register::Control, |ctl| {
            SbRmiControl(ctl)
                .with_block_read_write_en(value)
                .with_mbox_completion_software_alert_en(false)
                .0
        })
    }
}

impl SbRmi<ByteProto> {
    pub fn into_block_proto(self) -> Result<SbRmi<BlockProto>> {
        self.set_block_xfer_en(true)?;
        Ok(SbRmi {
            proto: PhantomData,
            device: self.device,
        })
    }

    fn read_mbox_out_data(&self) -> Result<u32> {
        let mut bs: [u8; 4] = [0u8; 4];
        bs[3] = self.read_byte_reg(Register::OutboundMsg4)?;
        bs[2] = self.read_byte_reg(Register::OutboundMsg3)?;
        bs[1] = self.read_byte_reg(Register::OutboundMsg2)?;
        bs[0] = self.read_byte_reg(Register::OutboundMsg1)?;
        Ok(u32::from_le_bytes(bs))
    }

    fn check_mailbox_command_status(&self) -> Result<MailboxErrorCode> {
        let code = match self.read_byte_reg(Register::OutboundMsg7)? {
            0x00 => MailboxErrorCode::Success,
            0x01 => MailboxErrorCode::CommandAborted,
            0x02 => MailboxErrorCode::UnknownCommand,
            0x03 => MailboxErrorCode::InvalidCore,
            0x05 => MailboxErrorCode::CommandFailedWithError(
                self.read_mbox_out_data()?,
            ),
            0x09 => MailboxErrorCode::InvalidInputArguments,
            0x0a => MailboxErrorCode::InvalidOobRasConfig,
            0x0b => MailboxErrorCode::DataNotReady,
            unk => MailboxErrorCode::UnknownError(unk),
        };
        Ok(code)
    }

    pub fn mailbox(&self, mbox_cmd: MailboxCmd) -> Result<Option<u32>> {
        let cmd = DecodedCommand::from(mbox_cmd);
        self.set_block_xfer_en(false)?;
        self.rmw_byte_reg(Register::Status, |status| {
            SbRmiStatus(status)
                .with_software_alert_status(true) // 1 clears
                .0
        })?;
        self.write_byte_reg(Register::InboundMsg7, 0x80)?;
        self.write_byte_reg(Register::InboundMsg0, cmd.cmd)?;
        if let Some(argument) = cmd.argument {
            let bs = argument.to_le_bytes();
            self.write_byte_reg(Register::InboundMsg4, bs[3])?;
            self.write_byte_reg(Register::InboundMsg3, bs[2])?;
            self.write_byte_reg(Register::InboundMsg2, bs[1])?;
            self.write_byte_reg(Register::InboundMsg1, bs[0])?;
        }
        self.write_byte_reg(Register::SoftwareIntr, 1)?;
        while self.read_byte_reg(Register::SoftwareIntr)? != 0 {
            core::hint::spin_loop();
        }
        match self.check_mailbox_command_status()? {
            MailboxErrorCode::Success => {}
            code => return Err(Error::MailboxCmdFailed { code }),
        }
        let result = if cmd.has_result {
            self.read_mbox_out_data()?
        } else {
            0
        };
        let respcmd = self.read_byte_reg(Register::OutboundMsg0)?;
        if respcmd != cmd.cmd {
            return Err(Error::MailboxResponseMismatch {
                wanted: cmd.cmd,
                got: respcmd,
            });
        }
        self.rmw_byte_reg(Register::Status, |status| {
            SbRmiStatus(status)
                .with_software_alert_status(true) // 1 clears
                .0
        })?;
        Ok(cmd.has_result.then_some(result))
    }

    pub fn nthreads(&self) -> Result<u32> {
        let lo = u32::from(self.read_byte_reg(Register::ThreadNumberLo)?);
        let hi = u32::from(self.read_byte_reg(Register::ThreadNumberHi)?);
        Ok((hi << 8) + lo)
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

    pub fn alert(&self) -> Result<[u8; MCE_ALERT_STATUS_SIZE]> {
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
                    let threadno = reg2thread(bank, bank_reg, bit);
                    let index = threadno / MCE_ALERT_STATUS_SIZE;
                    let offset = threadno % 8;
                    rval[index] |= 1 << offset;
                }
            }
        }

        Ok(rval)
    }
}

impl SbRmi<BlockProto> {
    pub fn into_byte_proto(self) -> Result<SbRmi<ByteProto>> {
        self.set_block_xfer_en(false)?;
        self.write_byte_reg(Register::ReadSize, 1)?;
        Ok(SbRmi {
            proto: PhantomData,
            device: self.device,
        })
    }

    fn read_block_reg(&self, reg: &[u8], result: &mut [u8]) -> Result<usize> {
        let cmd = [reg[0], reg[1]];
        let len = result.len() as u8 - 1;

        self.device.write(reg).map_err(|code| {
            Error::BadRegisterBlockWrite {
                reg: cmd,
                len: len,
                code: code,
            }
        })?;

        self.set_block_xfer_en(false)?;
        self.write_byte_reg(Register::ReadSize, len)?;
        self.write_byte_reg(Register::SoftwareIntr, 1)?;
        loop {
            let intr = self.read_byte_reg(Register::SoftwareIntr)?;
            if intr == 0 {
                break;
            }
        }
        self.set_block_xfer_en(true)?;
        self.device.read_reg_into(cmd, result).map_err(|code| {
            Error::BadRegisterBlockRead {
                reg: cmd,
                len: len,
                code: code,
            }
        })?;
        Ok(result[0] as usize)
    }

    #[allow(dead_code)]
    fn write_block_reg(&self, reg: &[u8]) -> Result<()> {
        self.device.write(reg).map_err(|code| {
            let cmd = [reg[0], reg[1]];
            let len = reg[2];
            Error::BadRegisterBlockWrite {
                reg: cmd,
                len: len,
                code: code,
            }
        })
    }

    pub fn cpuid(
        &self,
        thread: u32,
        eax: u32,
        ecx: u32,
    ) -> Result<CpuidResult> {
        // We need to do two reads to return the full set of registers.
        let mut cpuid: CpuidResult = Default::default();
        for hihalf in [false, true] {
            let mut result = [0u8; 10];
            let thr = thread
                .try_into()
                .map_err(|_| Error::BadThreadId { thread: thread })?;
            let req = CpuidRead::new(thr, eax, ecx as u8, hihalf);
            let reg = req.as_reg()?;
            ringbuf_entry!(Trace::CpuidCall(reg));
            self.read_block_reg(&reg, &mut result)?;
            ringbuf_entry!(Trace::CpuidResult(result));
            if result[0] != 9 {
                return Err(Error::BadCpuidLength { length: result[0] });
            }
            let code = u8_into_status_code(result[1]);
            if code != StatusCode::Success {
                return Err(Error::CpuidFailed { code });
            }
            let bs = &result[2..10];
            if !hihalf {
                cpuid.eax = u32::from_le_bytes([bs[0], bs[1], bs[2], bs[3]]);
                cpuid.ebx = u32::from_le_bytes([bs[4], bs[5], bs[6], bs[7]]);
            } else {
                cpuid.ecx = u32::from_le_bytes([bs[0], bs[1], bs[2], bs[3]]);
                cpuid.edx = u32::from_le_bytes([bs[4], bs[5], bs[6], bs[7]]);
            }
        }
        Ok(cpuid)
    }

    pub fn rdmsr<V: FromBytes>(&self, thread: u32, msr: u32) -> Result<V> {
        let size = core::mem::size_of::<V>() as u8;
        let msr = msr.to_le_bytes();

        let thrlo = (thread & 0b0111_1111) << 1;
        let thrhi = (thread >> 7) & 0b1111_1111;
        let request: [u8; 11] = [
            0x73, // Read CPUID/Read Register Command Format
            0,
            0x7,         // Payload size
            size,        // Desired return size
            0x86,        // Read register command
            thrlo as u8, // Thread ID
            thrhi as u8,
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

        let code = u8_into_status_code(result[1]);

        if code != StatusCode::Success {
            return Err(Error::RdmsrFailed { code });
        }

        Ok(<V>::read_from_bytes(&result[2..2 + size as usize]).unwrap())
    }
}

impl<T: SbRmiMessageProto> core::fmt::Display for SbRmi<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "sbrmi: {}", &self.device)
    }
}

fn u8_into_status_code(code: u8) -> StatusCode {
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

#[derive(Copy, Clone, Debug, PartialEq)]
enum Trace {
    None,
    CpuidCall([u8; 12]),
    CpuidResult([u8; 10]),
    RdmsrCall([u8; 11]),
    RdmsrResult([u8; 10]),
}

ringbuf!(Trace, 12, Trace::None);

pub struct Sbrmi21;

impl Validate<Error> for Sbrmi21 {
    fn validate(device: &I2cDevice) -> Result<bool> {
        let sbrmi = SbRmi::<ByteProto>::new(*device)?;
        let rev = sbrmi.read_byte_reg(Register::Revision)?;
        Ok(rev == 0x21)
    }
}
