//! Client API for the QSPI server
//!
//! An API for a Quad SPI controller and attached flash device.
//!
//! While probably general enough as an API, only the stm32h7 QUADSPI
//! controller and an attached Micron MT25Q family part are supported.
//!
//! TODO: Add GPIO control here or in the caller's code to control the Gimlet
//! SPI mux.

#![no_std]

use zerocopy::{AsBytes}; // XXX , FromBytes};

use ringbuf::*;
use userlib::*;

use core::cell::Cell;

#[derive(Debug, Copy, Clone, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum Op {
    Read = 1,   // Read with optional address and receive buffer
    Write = 2,  // Write with optional Address and transmit buffer
    Get = 3,    // A Read variant for any instruction that may return data
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Command {
    pub instruction: Instruction,
    pub direction: Direction,
    pub address: Option<u32>,     // 3 or 4 byte address in a u32.
    pub data_length: Option<u32>, // 0xffffffff is continuous until end of device.
}

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u32)]
pub enum ResponseCode {
    /// Malformed response
    BadResponse = 1,

    /// Bad argument
    BadArg = 2,

    /// Bad lease argument
    BadLeaseArg = 3,

    /// Bad lease attributes
    BadLeaseAttributes = 4,

    /// Bad source lease
    BadSource = 5,

    /// Bad source lease attibutes
    BadSourceAttributes = 6,

    /// Bad Sink lease
    BadSink = 7,

    /// Bad Sink lease attributes
    BadSinkAttributes = 8,

    /// Short sink length
    ShortSinkLength = 9,

    /// Bad lease count
    BadLeaseCount = 10,

    /// Transfer size is 0 or exceeds maximum
    BadTransferSize = 11,

    /// Could not transfer byte out of source
    BadSourceByte = 12,

    /// Could not transfer byte into sink
    BadSinkByte = 13,

    /// Server restarted
    ServerRestarted = 14,

    NotImplemented = 15,

    Busy = 16,
}

// Supported SPI Opcodes
//
// The set of legal opcodes and associated Qspi/Spi parameters can vary
// per supported flash device.
// These are taken from the Micron MT25Q datasheet but are meant to be
// symbolic here. If it were needed, they could be translated to device
// specific commands in the qspi driver.
//
// [Micron documentation](https://www.micron.com/-/media/client/global/documents/products/technical-note/nor-flash/tn2506_sfdp_for_mt25q.pdf).
#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum Instruction {
    // Note: These can be turned into simple abstract commands.
    //       They will be mapped to the Conntroller/FlashPart specific
    //       parameters for the controller in the driver.
    //
    // Taken from WinBond W25Q128JV datasheet.
    // We're not likely to use more than ten commands.
    //
    // Standard SPI Instructions
    //WEn = 0x06, // Write Enable 06h
    //VSRWEn = 0x50,  // Volatile SR Write Enable 50h
    //WDS = 0x04, // Write Disable 04h
    //RPD = 0xAB, // Release Power-down / ID ABh Dummy Dummy Dummy (ID7-ID0)(2)
    //MfgDId = 0x00,  // Manufacturer/Device ID 90h Dummy Dummy 00h (MF7-MF0) (ID7-ID0)
    JedecId = 0x9f,    // JEDEC ID 9Fh (MF7-MF0) (ID15-ID8) (ID7-ID0)
    RdUUID = 0x4B,   // Read Unique ID 4Bh Dummy Dummy Dummy Dummy (UID63-0)
    Read = 0x03,    // Read Data 03h A23-A16 A15-A8 A7-A0 (D7-D0)
    FastRead = 0x0B, // Fast Read 0Bh A23-A16 A15-A8 A7-A0 Dummy (D7-D0)
    PageProgram = 0x02,  // Page Program 02h A23-A16 A15-A8 A7-A0 D7-D0 D7-D0(3)
    SectorErase = 0x20,  // Sector Erase (4KB) 20h A23-A16 A15-A8 A7-A0
    //BE32K = 0x52,   // Block Erase (32KB) 52h A23-A16 A15-A8 A7-A0
    //BE64K = 0xD8,   // Block Erase (64KB) D8h A23-A16 A15-A8 A7-A0
    ChipErase = 0xC7,  // Chip Erase C7h/60h
    //CeAlt = 0x60,  // Chip Erase C7h/60h
    //RdSts1 = 0x05,  // Read Status Register-1 05h (S7-S0)(2)
    //WrSts1 = 0x01, // Write Status Register-1 (4) 01h (S7-S0)(4)
    //RdSts2 = 0x35, // Read Status Register-2 35h (S15-S8)(2)
    //WrSts2 = 0x31, // Write Status Register-2 31h (S15-S8)
    //RdSts3 = 0x15, // Read Status Register-3 15h (S23-S16)(2)
    //WrSts3 = 0x11, // Write Status Register-3 11h (S23-S16)
    //RdSfdp = 0x5a, // Read SFDP Register 5Ah 00 00 A7-A0 Dummy (D7-D0)
    //SecRegErase = 0x44,    //  Erase Security Register(5) 44h A23-A16 A15-A8 A7-A0
    //SecRegProg = 0x42, // Program Security Register(5) 42h A23-A16 A15-A8 A7-A0 D7-D0 D7-D0(3)
    //SecRegRd = 0x48,   // Read Security Register(5) 48h A23-A16 A15-A8 A7-A0 Dummy (D7-D0)
    //Glock = 0x7e,  // Global Block Lock 7Eh
    //GUnlocK = 0x98,    // Global Block Unlock 98h
    //RdBlkLock = 0x3D, // Read Block Lock 3Dh A23-A16 A15-A8 A7-A0 (L7-L0)
    //IBLock = 0x36, // Individual Block Lock 36h A23-A16 A15-A8 A7-A0
    //IBUnlock = 0x39,   // Individual Block Unlock 39h A23-A16 A15-A8 A7-A0
    //EraseProgSusp = 0x75, // Erase / Program Suspend 75h
    //EraseProgResume = 0x7A,   // Erase / Program Resume 7Ah
    //PwrDn = 0xB9,  // Power-down B9h
    //ResetEn = 0x66,    // Enable Reset 66h
    //Reset = 0x99,   // Reset Device 99h

    // Dual/Quad SPI Instructions
    // TODO: Add number of clocks per phase information
    //FRdDualOut = 0x3B,    // Fast Read Dual Output 3Bh A23-A16 A15-A8 A7-A0 Dummy Dummy (D7-D0) (7)
    //FRdDualIO = 0xBB, // Fast Read Dual I/O BBh A23-A16(6) A15-A8(6) A7-A0(6) Dummy(11) (D7-D0) (7)
    //MfgDevIdDual = 0x92,  // Mftr./Device ID Dual I/O 92h A23-A16(6) A15-A8(6) 00(6) Dummy(11) (MF7-MF0) (ID7-ID0)(7)
    //QIPP = 0x32,    // Quad Input Page Program 32h A23-A16 A15-A8 A7-A0 (D7-D0)(9) (D7-D0)(3) …
    //FRdQuadOut = 0x6B,    // Fast Read Quad Output 6Bh A23-A16 A15-A8 A7-A0 Dummy Dummy Dummy Dummy (D7-D0)(10)
    //MfgDevIdQuad = 0x94,  // Mftr./Device ID Quad I/O 94h A23-A16 A15-A8 00 Dummy(11) Dummy Dummy (MF7-MF0) (ID7-ID0)
    //FRdQuadIO = 0xEB, // Fast Read Quad I/O EBh A23-A16 A15-A8 A7-A0 Dummy(11) Dummy Dummy (D7-D0)
    //SetBurstWWrap = 0x77,    // Set Burst with Wrap 77h Dummy Dummy Dummy W8-W0
    Nop = 0x00,
}

impl From<u8> for Instruction {
    fn from(byte: u8) -> Self {
        match byte {
            0x02 => Instruction::PageProgram,
            0x03 => Instruction::Read,
            0x0B => Instruction::FastRead,
            0x20 => Instruction::SectorErase,
            0x9f => Instruction::JedecId,
            // 0x06 => Instruction::WriteEnable,
            // 0x04 => Instruction::WriteDisable,
            0x48 => Instruction::RdUUID,
            0xC7 => Instruction::ChipErase,
            _ => Instruction::Nop,
        }
    }
}

impl Instruction {

    pub fn is_read(&self) -> bool {
        match self {
            Instruction::Read |
            Instruction::FastRead |
            Instruction::JedecId |
            Instruction::RdUUID => true,
            _ => false,
        }
    }

    pub fn is_write(&self) -> bool {
        match self {
            Instruction::PageProgram => true,
            _ => false,
        }
    }

    pub fn requires_address(&self) -> bool {
        match self {
            Instruction::FastRead |
            Instruction::PageProgram |
            Instruction::Read |
            Instruction::SectorErase => true,
            _ => false,
        }
    }
}


#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u8)]
pub enum Direction {
    NoData = 0x00,
    Read = 0x01,
    Write = 0x02,
    Exchange = 0x03,
    Unknown = 0xff, // TODO: What is the better way?
}

impl From<u8> for Direction {
    fn from(byte: u8) -> Self {
        match byte {
            0x00 => Direction::NoData,
            0x01 => Direction::Read,
            0x02 => Direction::Write,
            0x03 => Direction::Exchange,
            _ => Direction::Unknown,
        }
    }
}

pub trait Marshal<T> {
    fn marshal(&self) -> T;
    fn unmarshal(val: &T) -> Result<Self, ResponseCode>
    where
        Self: Sized;
}

type Address = u32;
type DataLength = u32;
type CommandMsg = (Instruction, Option<Address>, Option<DataLength>);

impl Marshal<[u8; 9]> for CommandMsg {
    fn marshal(&self) -> [u8; 9] {
        let addr = match self.1 {
            Some(addr) => {
                let x = addr as u32;
                (((x >> 24) & 0x7f) as u8,
                ((x >> 16) & 0xff) as u8,
                ((x >> 8) & 0xff) as u8,
                (x & 0xff) as u8)
            },
            None => (0x80u8, 0u8, 0u8, 0u8),
        };
        let dlen = match self.2 {
            // btw, dlen.as_be_bytes() would help you here I think
            // let dlen = self.3
            //     .map(|x| x.as_be_bytes())
            //         .unwrap_or([0x8, 0x8, ...]);
            Some(dlen) => {
                // .map(x) x.to_be_bytes()
                let x = dlen as u32;
                (((x >> 24) & 0x7f) as u8,
                ((x >> 16) & 0xff) as u8,
                ((x >> 8) & 0xff) as u8,
                (x & 0xff) as u8)
            },
            // None => [0x80000000].to_be_bytes(),
            None => (0x80u8, 0u8, 0u8, 0u8),
        };

        [
            self.0 as u8,
            addr.0, addr.1, addr.2, addr.3,
            dlen.0, dlen.1, dlen.2, dlen.3,
        ]
    }

    fn unmarshal(val: &[u8;9]) -> Result<Self, ResponseCode> {
        let inst: Instruction = val[0].into();
        let maybe_address = u32::from(val[1]) << 24
            | u32::from(val[2]) << 16
            | u32::from(val[3]) << 8
            | u32::from(val[4]);
        let maybe_dlen = u32::from(val[5]) << 24
            | u32::from(val[6]) << 16
            | u32::from(val[7]) << 8
            | u32::from(val[8]);
        Ok((
                inst,
                if maybe_address == 0x80000000 { None } else { Some(maybe_address) },
                if maybe_dlen == 0x80000000 { None } else { Some(maybe_dlen) }
        ))
    }
}

impl Op {
    pub fn is_read(self) -> bool {
        self as u32 & 1 != 0
    }

    pub fn is_write(self) -> bool {
        self as u32 & 0b10 != 0
    }
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

#[derive(Clone, Debug)]
pub struct Qspi(Cell<TaskId>);

impl From<TaskId> for Qspi {
    fn from(t: TaskId) -> Self {
        Self(Cell::new(t))
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Read(TaskId, Instruction, Option<u32>, Option<u32>),
    Write(TaskId, Instruction, Option<u32>, Option<u32>),
    Result(TaskId, u32),
    None,
}

ringbuf!(Trace, 8, Trace::None);

// This structure is tied to the STM32h7 QUADSPI implemnetation.
// That said, the information is generic to the quadspi protocol and
// would at worst have several don't-cares for other controller implementations.
impl Qspi {

    // Read optional address and data length returning 0 to buffer length bytes.
    pub fn command_read(
        &self,
        instruction: Instruction,
        addr: Option<Address>,
        dlen: Option<DataLength>,
        buf: &mut [u8],
    ) -> Result<usize, ResponseCode> {
        // let mut val = V::default(); // XXX useful to return xfer size?
        let mut response = 0_usize; // not u32?
        let task = self.0.get();
        ringbuf_entry!(Trace::Read(task, instruction, addr, dlen));
        let (code, _) = sys_send(
            task,
            Op::Read as u16,
            &Marshal::marshal(&(
                    instruction,
                    addr,
                    dlen,
            )),
            response.as_bytes_mut(),
            &[Lease::from(buf)
            // , Lease::from(val.as_bytes_mut())],
            ],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(0)
        }
    }

    // Write optional address and data length returning 0 to buffer length bytes.
    pub fn command_write(
        &self,
        instruction: Instruction,
        addr: Option<Address>,
        dlen: Option<DataLength>,
        buf: &[u8],
    ) -> Result<usize, ResponseCode> {
        // let mut val = V::default(); // XXX useful to return xfer size?
        let mut response = 0_usize; // not u32?
        let task = self.0.get();
        ringbuf_entry!(Trace::Write(task, instruction, addr, dlen));
        let (code, _) = sys_send(
            task,
            Op::Write as u16,
            &Marshal::marshal(&(
                    instruction,
                    addr,
                    dlen,
            )),
            response.as_bytes_mut(),
            &[Lease::from(buf)
            // , Lease::from(val.as_bytes_mut())],
            ],
        );

        if code != 0 {
            Err(ResponseCode::from_u32(code)
                .ok_or(ResponseCode::BadResponse)?)
        } else {
            Ok(0)
        }
    }

    pub fn result(&self, task: TaskId, code: u32) -> Result<(), ResponseCode> {
        ringbuf_entry!(Trace::Result(task, code));
        if code != 0 {
            //
            // If we have an error code, check to see if it denotes a dearly
            // departed task; if it does, in addition to returning a specific
            // error code, we will set our task to be the new task as a courtesy.
            //
            if let Some(g) = abi::extract_new_generation(code) {
                self.0.set(TaskId::for_index_and_gen(task.index(), g));
                Err(ResponseCode::ServerRestarted)
            } else {
                Err(ResponseCode::from_u32(code).ok_or(ResponseCode::BadResponse)?)
            }
        } else {
            Ok(())
        }
    }
}
