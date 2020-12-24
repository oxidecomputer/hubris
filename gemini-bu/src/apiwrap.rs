//! Alternate register access sketch
//!
//! Principles:
//!
//! - Values must be read faithfully. The result of a read should contain all of
//!   the bits read, even if they're invalid.
//!
//! - If a field is extracted into a limited-domain type, such as an enum, and
//!   its value is invalid, we must notice. Return None or panic.
//!
//! - We'll try our best not to write invalid values to reserved bits. The type
//!   used to write must maintain WZ/WO and "keep at reset value" fields in
//!   their proper shape.
//!
//! - The "read and write it back with changes" pattern should be simple and
//!   cheap. In the case of reset values that must be preserved, etc., we'll
//!   need to replace the bits.

use core::marker::PhantomData;

pub struct PwrRegisters {
    pub cr1: Reg<CR1>,
    pub csr1: Reg<CSR1>,
    pub cr2: Reg<CR2>,
    pub cr3: Reg<CR3>,
//    pub cpucr: CPUCR,
//    _reserved5: [u8; 4usize],
//    pub d3cr: D3CR,
//    _reserved6: [u8; 4usize],
//    pub wkupcr: WKUPCR,
//    pub wkupfr: WKUPFR,
//    pub wkupepr: WKUPEPR,
}

impl PwrRegisters {
    pub unsafe fn get() -> &'static Self {
        &*(0x5802_4800 as *const _)
    }
}

/////////////////////////////////
// CR1 definition

pub enum CR1 {}

impl RegPersonality for CR1 {
    type Rep = u32;
}

impl CanRead for CR1 {
    type R = Cr1Value;
}

#[derive(Copy, Clone)]
pub struct Cr1Value(u32);

impl From<u32> for Cr1Value {
    fn from(bits: u32) -> Self {
        Cr1Value(bits)
    }
}

/////////////////////////////////
// CSR1 definition

pub enum CSR1 {}

impl RegPersonality for CSR1 {
    type Rep = u32;
}

impl CanRead for CSR1 {
    type R = Csr1Value;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Csr1Value(u32);

impl From<u32> for Csr1Value {
    fn from(bits: u32) -> Self {
        Csr1Value(bits)
    }
}

impl Csr1Value {
    pub fn mmcvdo(self) -> bool {
        self.0 & (1 << 17) != 0
    }
    pub fn avdo(self) -> bool {
        self.0 & (1 << 16) != 0
    }
    pub fn actvosrdy(self) -> bool {
        self.0 & (1 << 13) != 0
    }
    pub fn pvdo(self) -> bool {
        self.0 & (1 << 4) != 0
    }
}

/////////////////////////////////
// CR2 definition

pub enum CR2 {}

impl RegPersonality for CR2 {
    type Rep = u32;
}

impl CanRead for CR2 {
    type R = Cr2Value;
}

#[derive(Copy, Clone)]
pub struct Cr2Value(u32);

impl From<u32> for Cr2Value {
    fn from(bits: u32) -> Self {
        Cr2Value(bits)
    }
}

/////////////////////////////////
// CR3 definition

pub enum CR3 {}

impl RegPersonality for CR3 {
    type Rep = u32;
}

impl CanRead for CR3 {
    type R = Cr3Value;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Cr3Value(u32);

impl From<u32> for Cr3Value {
    fn from(bits: u32) -> Self {
        Cr3Value(bits)
    }
}

impl Cr3Value {
    pub fn usb33rdy(self) -> bool {
        self.0 & (1 << 26) != 0
    }
    pub fn usbregen(self) -> bool {
        self.0 & (1 << 25) != 0
    }
    pub fn usb33den(self) -> bool {
        self.0 & (1 << 24) != 0
    }
    pub fn smpsextrdy(self) -> bool {
        self.0 & (1 << 16) != 0
    }
    pub fn vbrs(self) -> bool {
        self.0 & (1 << 9) != 0
    }
    pub fn vbe(self) -> bool {
        self.0 & (1 << 8) != 0
    }
    pub fn smpslevel(self) -> SmpsLevel {
         match (self.0 >> 4) & 0b11 {
             0b00 => SmpsLevel::ResetValue,
             0b01 => SmpsLevel::V1_8,
             0b10 => SmpsLevel::V2_5,
             _ => SmpsLevel::V2_5a,
         }
    }
    pub fn smpsexthp(self) -> SmpsExtHP {
        if self.0 & (1 << 3) != 0 {
            SmpsExtHP::External
        } else {
            SmpsExtHP::Normal
        }
    }
    pub fn smpsen(self) -> bool {
        self.0 & (1 << 2) != 0
    }
    pub fn ldoen(self) -> bool {
        self.0 & (1 << 1) != 0
    }
    pub fn bypass(self) -> bool {
        self.0 & (1 << 0) != 0
    }
}

impl CanWrite for CR3 {
    type W = Cr3Update;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Cr3Update(u32);

impl From<Cr3Update> for u32 {
    fn from(x: Cr3Update) -> Self {
        x.0
    }
}

impl Cr3Update {
    pub fn with_usbregen(self, v: bool) -> Self {
        Self(self.0 & !(1 << 25) | u32::from(v) << 25)
    }
    pub fn with_usb33den(self, v: bool) -> Self {
        Self(self.0 & !(1 << 24) | u32::from(v) << 24)
    }
    pub fn with_vbrs(self, v: bool) -> Self {
        Self(self.0 & !(1 << 9) | u32::from(v) << 9)
    }
    pub fn with_vbe(self, v: bool) -> Self {
        Self(self.0 & !(1 << 8) | u32::from(v) << 8)
    }
    pub fn with_smpslevel(self, v: SmpsLevel) -> Self {
        Self(self.0 & !(0b11 << 4) | (v as u32) << 4)
    }
    pub fn with_smpsexthp(self, v: SmpsExtHP) -> Self {
        Self(self.0 & !(1 << 3) | (v as u32) << 3)
    }
    pub fn with_smpsen(self, v: bool) -> Self {
        Self(self.0 & !(1 << 2) | u32::from(v) << 2)
    }
    pub fn with_ldoen(self, v: bool) -> Self {
        Self(self.0 & !(1 << 1) | u32::from(v) << 1)
    }
    pub fn with_bypass(self, v: bool) -> Self {
        Self(self.0 & !(1 << 0) | u32::from(v) << 0)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SmpsLevel {
    ResetValue = 0b00,
    V1_8 = 0b01,
    V2_5 = 0b10,
    V2_5a = 0b11,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SmpsExtHP {
    Normal = 0,
    External = 1,
}

impl CanModify for CR3 {
    fn write_from_read(r: Self::R) -> Self::W {
        Cr3Update(r.0 & 0x0701_033f)
    }
}

/////////////////////////////////
// Generic register support code

pub struct Reg<Pers: RegPersonality> {
    inner: vcell::VolatileCell<Pers::Rep>,
}

pub trait RegPersonality {
    type Rep: Copy;
}

pub trait CanRead: RegPersonality {
    type R: From<Self::Rep> + Copy;
}

pub trait CanWrite: RegPersonality {
    type W: Into<Self::Rep>;
}

pub trait CanModify: CanRead + CanWrite {
    fn write_from_read(_: Self::R) -> Self::W;
}

impl<Pers: RegPersonality> Reg<Pers> {
    pub fn read(&self) -> Pers::R where Pers: CanRead {
        Pers::R::from(self.inner.get())
    }
    pub fn write(&self, value: Pers::W) where Pers: CanWrite {
        self.inner.set(value.into());
    }
    pub fn modify(&self, f: impl FnOnce(Pers::R, Pers::W) -> Pers::W)
        where Pers: CanModify
    {
        let r = self.read();
        self.write(f(r, Pers::write_from_read(r)))
    }
}
