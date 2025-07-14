// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Client API for SB-RMI driver

#![no_std]

use drv_i2c_api::ResponseCode;
use userlib::*;

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
    counters::Count,
)]
pub enum SbRmi21Error {
    Unavailable,
    Unsupported,
    BusLocked,
    CpuidUnavailable,
    CpuidTimeout,
    I2cError,
    BadRegisterRead {
        reg: u8,
        code: ResponseCode,
    },
    BadRegisterWrite {
        reg: u8,
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
        thread: u32,
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
    MailboxResponseMismatch {
        wanted: u8,
        got: u8,
    },
    MailboxCmdFailed {
        code: SbRmi21MailboxErrorCode,
    },
}

impl From<SbRmi21Error> for ResponseCode {
    fn from(err: SbRmi21Error) -> Self {
        match err {
            SbRmi21Error::BadRegisterRead { code, .. } => code,
            SbRmi21Error::BadRegisterWrite { code, .. } => code,
            SbRmi21Error::BadRegisterBlockWrite { code, .. } => code,
            SbRmi21Error::BadCpuidRead { code } => code,
            SbRmi21Error::BadRdmsr { code } => code,
            _ => ResponseCode::BadResponse,
        }
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
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

/// APML works in power limits in units of mW.
#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct Power(u32);

impl Power {
    pub const fn milliwatts(self) -> u32 {
        self.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct BoostFreqLimit(u32);

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct ApmlFreqLimit(u16);

impl From<ApmlFreqLimit> for u32 {
    fn from(freq_lim: ApmlFreqLimit) -> u32 {
        u32::from(freq_lim.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct ApmlLogicalCoreId(u16);

impl From<ApmlLogicalCoreId> for u32 {
    fn from(coreid: ApmlLogicalCoreId) -> u32 {
        u32::from(coreid.0) << 16
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct DramThrottle(u32);

impl DramThrottle {
    pub fn from_percent(throttle: u8) -> DramThrottle {
        DramThrottle(if throttle > 80 { 80 } else { throttle.into() })
    }
}

impl From<DramThrottle> for u32 {
    fn from(throttle: DramThrottle) -> u32 {
        throttle.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct CcdInst(u32);

impl From<CcdInst> for u32 {
    fn from(inst: CcdInst) -> u32 {
        inst.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct CcxInst(u32);

impl From<CcxInst> for u32 {
    fn from(inst: CcxInst) -> u32 {
        inst.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct FuseSamplingCounter(u32);

impl From<FuseSamplingCounter> for u32 {
    fn from(counter: FuseSamplingCounter) -> u32 {
        counter.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(u32)]
pub enum HiLo {
    Lo = 0,
    Hi = 1,
}

impl From<HiLo> for u32 {
    fn from(hl: HiLo) -> u32 {
        hl as u32
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct PostCodeOffset(u32);

impl From<PostCodeOffset> for u32 {
    fn from(pco: PostCodeOffset) -> u32 {
        pco.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(u32)]
pub enum SerialNoCategory {
    Ccd = 0,
    Iod = 1,
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(transparent)]
pub struct ChipletNumber(u8);

impl From<ChipletNumber> for u32 {
    fn from(chipletno: ChipletNumber) -> u32 {
        chipletno.0.into()
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
#[repr(u32)]
pub enum SpdSbBusOvrdOp {
    Set = 0,
    Get = 1,
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct Temperature(u32);

impl Temperature {
    pub fn from_degrees_celsius(degc: u32) -> Temperature {
        Temperature(degc)
    }
    pub fn degrees_celsius(self) -> u32 {
        self.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct Current(u32);

impl Current {
    pub fn from_milliamps(ma: u32) -> Self {
        Self(ma)
    }
    pub fn milliamps(self) -> u32 {
        self.0
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct DimmAddr(u8);

impl From<DimmAddr> for u32 {
    fn from(da: DimmAddr) -> u32 {
        u32::from(da.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct UpdateRate(u16);

impl UpdateRate {
    pub fn from_millisec(ms: u16) -> Self {
        Self(ms)
    }
    pub fn millisec(self) -> u32 {
        u32::from(self.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct PcieSegment(u8);

impl From<PcieSegment> for u32 {
    fn from(seg: PcieSegment) -> u32 {
        u32::from(seg.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct DwordNo(u16);

impl DwordNo {
    pub fn offset(self) -> u32 {
        u32::from(self.0) * 4
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct Bus(u8);

impl From<Bus> for u32 {
    fn from(bus: Bus) -> u32 {
        u32::from(bus.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct Device(u8);

impl From<Device> for u32 {
    fn from(dev: Device) -> u32 {
        u32::from(dev.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct Function(u8);

impl From<Function> for u32 {
    fn from(func: Function) -> u32 {
        u32::from(func.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub struct McaBank(u16);

impl From<McaBank> for u32 {
    fn from(bank: McaBank) -> u32 {
        u32::from(bank.0)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub enum SbRmi21MailboxCmd {
    ReadPkgPwrConsumption,
    WritePkgPwrLim(Power),
    ReadPkgPwrLim,
    ReadMaxPkgPwrLim,
    ReadTdp,
    ReadMaxcTdp,
    ReadMincTdp,
    ReadBiosBoostFMax(ApmlLogicalCoreId),
    ReadApmlBoostLim(ApmlLogicalCoreId),
    WriteApmlBoostLim(ApmlLogicalCoreId, ApmlFreqLimit),
    WriteApmlBoostLimAllCores(ApmlFreqLimit),
    ReadDramThrottle,
    WriteDramThrottle(DramThrottle),
    ReadProchotStatus,
    ReadProchotResidency,
    ReadIodBistResult,
    ReadCcdBistResult(CcdInst),
    ReadCcxBistResult(CcxInst),
    ReadCclkFreqLim,
    ReadSockC0Residency,
    GetMaxDdrBwAndUtil,
    GetMp1FirmwareVers,
    InitFuseSample(FuseSamplingCounter),
    ReadFuseSettings,
    ReadPpinFuse(HiLo),
    ReadPostCode(PostCodeOffset),
    ReadRtc(HiLo),
    ReadPubDieSerNo(HiLo, SerialNoCategory, ChipletNumber),
    SpdSidebandBusClearOverride(SpdSbBusOvrdOp, bool), // Documented as temporary. Do not use.
    WriteFastPptLim(Power),
    WriteThermCtlLim(Temperature),
    WriteVrmVddCurrentLim(Current),
    WriteVrmVddMaxCurrentLim(Current),
    BmcReportDimmPwrConsumption(Power, UpdateRate, DimmAddr),
    BmcReportDimmThermSensor(Temperature, UpdateRate, DimmAddr),
    BmcRasPcieConfigAccess(PcieSegment, DwordNo, Bus, Device, Function),
    BmcRasMcaValidityCheck,
    BmcRasMcaMsrDump(McaBank, DwordNo),
}

#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    serde::Deserialize,
    serde::Serialize,
    hubpack::SerializedSize,
)]
pub enum SbRmi21MailboxErrorCode {
    Success,
    CommandAborted,
    UnknownCommand,
    InvalidCore,
    CommandFailedWithError(u32),
    InvalidInputArguments,
    InvalidOobRasConfig,
    DataNotReady,
    UnknownError(u8),
}

include!(concat!(env!("OUT_DIR"), "/client_stub.rs"));
