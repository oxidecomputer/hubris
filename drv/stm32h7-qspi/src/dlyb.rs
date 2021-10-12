//! A driver for the STM32H7 QUADSPI, in host mode.
//! (See ST document RM0433 section 24)

// See https://docs.rs/stm32h7/0.14.0/stm32h7/stm32h747cm7/delay_block_sdmmc1/index.html
//
// RM0433 Delay block (DLBY)
//
// The delay block (DLYB) is used to generate an output clock which
// is dephased from the input clock.
// The phase of the output clock must be programmed by the user
// application. The output clock is then used to clock the data received
// by another peripheral such as an SDMMC or Quad-SPI interface.
// 
// The delay is voltage- and temperature-dependent, which may require
// the application to reconfigure and recenter the output clock phase with
// the receive data.

// Signal name, Signal type,    Description
// dlyb_hclk,   Digital input,  Delay block register interface clock
// dlyb_in_ck,  Digital input,  Delay block input clock
// dlyb_out_ck, Digital output, Delay block output clock


use stm32h7::stm32h743 as device;

pub struct Dlyb {
    reg: &'static device::delay_block_sdmmc1::RegisterBlock,
}

impl From<&'static device::delay_block_sdmmc1::RegisterBlock> for Dlyb {
    fn from(reg: &'static device::delay_block_sdmmc1::RegisterBlock) -> Self {
        Self { reg }
    }
}

// TODO: Get these (equivalents) into stm32h7.rs upstream.

// Sampler length enable bit.
// 0: Sampler len and reg access to UNIT and SEL disabled, output clock enabled.
// 1: Sampler len and reg access to UNIT and SEL enabled, output clock disabled.
const DLYB_CR_SEN_SHIFT: u32 = 1;
const DLYB_CR_SEN_MASK: u32 = 1 << 1;
//
// Delay block enable bit.
// 0: Delay block disabled.
// 1: Delay block disabled.
const DLYB_CR_DEN_SHIFT: u32 = 1;
const DLYB_CR_DEN_MASK: u32 = 1 << 0;

// Length valid field
const DLYB_CFGR_LNGF_SHIFT: u32 = 31;
const DLYB_CFGR_LNGF_MASK: u32 = 1 << 31;

// Delay line length value
// These bits reflec the 12 unit delay values sampled at the rising edge of
// the input clock.
const DLYB_CFGR_LNG_SHIFT: u32 = 16;
const DLYB_CFGR_LNG_MASK: u32 = 0xfff << 16;

// Delay defines the delay of a unit delay cell.
// These bits can only be written when SEN=1.
// Unit delay = initial delay + UNIT x delay step.
const DLYB_CFGR_UNIT_SHIFT: u32 = 8;
const DLYB_CFGR_UNIT_MASK: u32 = 0x7f << 8;

// Select the phase for the output clock.
// These bits can only be written when SEN=1.
// Output clock phase = Input clock + SEL x Unit delay.
//
const DLYB_CFGR_SEL_SHIFT: u32 = 8;
const DLYB_CFGR_SEL_MASK: u32 = 0xf << 0;

impl Dlyb {
    pub fn get_cr_cfgr(&self) -> (u32, u32) {
        unsafe {
        (self.reg.cr.read().bits(), self.reg.cfgr.read().bits())
        }
    }

    pub fn enable_sen(&self) {
        unsafe {
        self.reg.cr.write(|w| w.bits(self.reg.cr.read().bits() | DLYB_CR_SEN_MASK))
        }
    }

    pub fn disable_sen(&self) {
        unsafe {
        self.reg.cr.write(|w| w.bits(self.reg.cr.read().bits() & !DLYB_CR_SEN_MASK))
        }
    }

    pub fn enable_den(&self) {
        unsafe {
        self.reg.cr.write(|w| w.bits(self.reg.cr.read().bits() | DLYB_CR_DEN_MASK))
        }
    }

    pub fn disable_den(&self) {
        unsafe {
        self.reg.cr.write(|w| w.bits(self.reg.cr.read().bits() & !DLYB_CR_DEN_MASK))
        }
    }

    pub fn set_lngf_lng_unit_sel(&self, lngf: u32, lng: u32, unit: u32, sel: u32) {
        unsafe {
        self.reg.cfgr.write(|w| w.bits(
                (self.reg.cfgr.read().bits() & !(DLYB_CFGR_LNGF_MASK | DLYB_CFGR_LNG_MASK | DLYB_CFGR_UNIT_MASK | DLYB_CFGR_SEL_MASK)) |
                ((DLYB_CFGR_LNGF_MASK & (lngf << DLYB_CFGR_LNGF_SHIFT)) | (DLYB_CFGR_LNG_MASK & (lng << DLYB_CFGR_LNG_SHIFT)) | (DLYB_CFGR_UNIT_MASK & (unit << DLYB_CFGR_UNIT_SHIFT)) | (DLYB_CFGR_SEL_MASK & (sel << DLYB_CFGR_SEL_SHIFT)))));
        }
    }
}
