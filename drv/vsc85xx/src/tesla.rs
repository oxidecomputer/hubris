// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::util::detype;
use crate::Trace;
use crate::{Phy, PhyRw};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use ringbuf::ringbuf_entry_root as ringbuf_entry;
use vsc7448_pac::{phy, types::PhyRegisterAddress};
use vsc_err::VscError;

pub struct TeslaPhy<'a, 'b, P> {
    pub phy: &'b mut Phy<'a, P>,
}

// "VTSS_TESLA_MCB_CFG_BUF_START_ADDR"
const MCB_CFG_BUF_START_ADDR: u16 = 0xd7c7;

impl<'a, 'b, P: PhyRw> TeslaPhy<'a, 'b, P> {
    /// Applies a patch to the 8051 microcode inside the PHY, based on
    /// `vtss_phy_pre_init_seq_tesla_rev_e` in the SDK
    pub(crate) fn patch(&mut self) -> Result<(), VscError> {
        ringbuf_entry!(Trace::TeslaPatch(self.phy.port));

        // Enable broadcast flag to configure all ports simultaneously
        self.phy
            .modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
                *r = (u16::from(*r) | 1).into();
            })?;

        self.phy
            .write(phy::STANDARD::EXTENDED_PHY_CONTROL_2(), 0x0040.into())?;
        self.phy
            .write(phy::EXTENDED_2::CU_PMD_TX_CTRL(), 0x02be.into())?;
        self.phy.write(phy::TEST::TEST_PAGE_20(), 0x4320.into())?;
        self.phy.write(phy::TEST::TEST_PAGE_24(), 0x0c00.into())?;
        self.phy.write(phy::TEST::TEST_PAGE_9(), 0x18ca.into())?;
        self.phy.write(phy::TEST::TEST_PAGE_5(), 0x1b20.into())?;

        // "Enable token-ring during coma-mode"
        self.phy.modify(phy::TEST::TEST_PAGE_8(), |r| {
            r.0 |= 0x8000;
        })?;

        for ((page, addr), value) in TESLA_TR_CONFIG {
            self.phy.write(
                PhyRegisterAddress::from_page_and_addr_unchecked(page, addr),
                value,
            )?;
        }

        self.phy.modify(phy::TEST::TEST_PAGE_8(), |r| {
            r.0 &= !0x8000; // Disable token-ring mode
        })?;

        self.phy
            .modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
                *r = (u16::from(*r) & !1).into();
            })?;

        //////////////////////////////////////////////////////////////////////////
        // Now we're going deep into the weeds.  This section is based on
        // `tesla_revB_8051_patch` in the SDK, which (as the name suggests), patches
        // the 8051 in the PHY.
        const FIRMWARE_START_ADDR: u16 = 0x4000;
        const PATCH_CRC_LEN: u16 = (TESLA_PATCH.len() + 1) as u16;
        const EXPECTED_CRC: u16 = 0x29E8;

        // This patch can only be applied to Port 0 of the PHY, so we'll check
        // the address here.
        let phy_port =
            self.phy.read(phy::EXTENDED::EXTENDED_PHY_CONTROL_4())?.0 >> 11;
        if phy_port != 0 {
            return Err(VscError::BadPhyPatchPort(phy_port));
        }
        let crc = self.phy.read_8051_crc(FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
        let skip_download = crc == EXPECTED_CRC;
        let patch_ok = skip_download
            && self.phy.read(phy::GPIO::GPIO_3())?.0 == 0x3eb7
            && self.phy.read(phy::GPIO::GPIO_4())?.0 == 0x4012
            && self.phy.read(phy::GPIO::GPIO_12())?.0 == 0x0100
            && self.phy.read(phy::GPIO::GPIO_0())?.0 == 0xc018;

        ringbuf_entry!(Trace::PatchState {
            patch_ok,
            skip_download
        });

        if !skip_download || !patch_ok {
            self.phy.micro_assert_reset()?;
        }
        if !skip_download {
            self.phy.download_patch(&TESLA_PATCH)?;
        }
        if !patch_ok {
            // Various CPU commands to enable the patch
            self.phy.write(phy::GPIO::GPIO_3(), 0x3eb7.into())?;
            self.phy.write(phy::GPIO::GPIO_4(), 0x4012.into())?;
            self.phy.write(phy::GPIO::GPIO_12(), 0x0100.into())?;
            self.phy.write(phy::GPIO::GPIO_0(), 0xc018.into())?;
        }

        if !skip_download {
            let crc =
                self.phy.read_8051_crc(FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
            if crc != EXPECTED_CRC {
                return Err(VscError::PhyPatchFailedCrc);
            }
        }

        //////////////////////////////////////////////////////////////////////////
        // `vtss_phy_pre_init_tesla_revB_1588`
        //
        // "Pass the cmd to Micro to initialize all 1588 analyzer registers to
        //  default"
        self.phy.cmd(0x801A)?;

        Ok(())
    }

    pub fn read_patch_settings(
        &mut self,
    ) -> Result<TeslaSerdes6gPatch, VscError> {
        // Based on vtss_phy_tesla_patch_setttings_get_private
        // This is not a place of honor.

        let mut cfg = [0; 38];
        let mcb_bus = 1; // "only 6G macros used for QSGMII MACs"
        let slave_num = 0;

        // "Read MCB macro into PRAM" (line 3994)
        self.phy.cmd(0x8003 | (slave_num << 8) | (mcb_bus << 4))?;

        self.phy.cmd(MCB_CFG_BUF_START_ADDR)?; // Line 3998

        for byte in &mut cfg {
            // "read the value of cfg_buf[idx] w/ post-incr."
            self.phy.cmd(0x9007)?;
            let r = self.phy.read(phy::GPIO::MICRO_PAGE())?;

            // "get bits 11:4 from return value"
            *byte = (u16::from(r) >> 4) as u8;
        }
        Ok(TeslaSerdes6gPatch { cfg })
    }

    pub fn tune_serdes6g_ob(
        &mut self,
        cfg: TeslaSerdes6gObConfig,
    ) -> Result<(), VscError> {
        cfg.check_range()?;

        let mcb_bus = 1; // "only 6G macros used for QSGMII MACs"
        let slave_num = 0;

        // Line 4967
        self.phy.cmd(0x8003 | (slave_num << 8) | (mcb_bus << 4))?;

        self.write_patch_value(77..=82, cfg.ob_post0)?;
        self.write_patch_value(72..=76, cfg.ob_post1)?;
        self.write_patch_value(67..=71, cfg.ob_prec)?;
        self.write_patch_value(62..=62, cfg.ob_sr_h)?;
        self.write_patch_value(54..=57, cfg.ob_sr)?;

        // "Write MCB for 6G macro 0 from PRAM" (line 4982)
        self.phy.cmd(0x9c40)?;
        Ok(())
    }

    pub fn read_serdes6g_ob(
        &mut self,
    ) -> Result<TeslaSerdes6gObConfig, VscError> {
        let mcb_bus = 1; // "only 6G macros used for QSGMII MACs"
        let slave_num = 0;
        self.phy.cmd(0x8003 | (slave_num << 8) | (mcb_bus << 4))?;

        let ob_post0 = self.read_patch_value(77..=82)?;
        let ob_post1 = self.read_patch_value(72..=76)?;
        let ob_prec = self.read_patch_value(67..=71)?;
        let ob_sr_h = self.read_patch_value(62..=62)?;
        let ob_sr = self.read_patch_value(54..=57)?;

        Ok(TeslaSerdes6gObConfig {
            ob_post0,
            ob_post1,
            ob_prec,
            ob_sr_h,
            ob_sr,
        })
    }

    /// Writes a single value to the TESLA patch region config array
    ///
    /// Loosely based on `patch_array_set_value`, but not _terrible_.
    fn write_patch_value(
        &mut self,
        bits: core::ops::RangeInclusive<u16>,
        value: u8,
    ) -> Result<(), VscError> {
        let bit_size: u16 = bits.end() - bits.start() + 1;
        assert!(bit_size <= 8);

        // Build a right-aligned mask, e.g. 0b0011111 or 0b00000001
        //
        // This uses checked and wrapping operations  to correctly handle the
        // case where bit_size == 8, which shifts the 1 out then underflows to
        // 0b11111111
        let mask: u8 = 1u8
            .checked_shl(bit_size as u32)
            .unwrap_or(0)
            .wrapping_sub(1);

        // Shift the mask and value into a u16, to handle cases where we
        // straddle a boundary between bytes.
        let bit_start = bits.start() % 8;
        let mut mask = u16::from(mask) << bit_start;
        let mut value = u16::from(value) << bit_start;

        // Set the start address
        let addr = MCB_CFG_BUF_START_ADDR + bits.start() / 8;
        self.phy.cmd(addr)?;

        while mask != 0 {
            self.phy.cmd(0x8007)?; // Read cfg_buffer[byte], no post-increment

            // Read the actual byte from the config vuffer
            let r = self.phy.read(phy::GPIO::MICRO_PAGE())?;
            let mut r = (u16::from(r) >> 4) as u8;

            // Modify the byte, then prepare to handle the next byte
            r = (r & !(mask as u8)) | (value as u8);
            mask >>= 8;
            value >>= 8;

            // Write the data back, with post-increment
            self.phy.cmd(0x9006 | (u16::from(r) << 4))?;
        }
        Ok(())
    }

    /// Reads a single value from the TESLA patch region config array
    fn read_patch_value(
        &mut self,
        bits: core::ops::RangeInclusive<u16>,
    ) -> Result<u8, VscError> {
        // Set the start address
        let addr = MCB_CFG_BUF_START_ADDR + bits.start() / 8;
        self.phy.cmd(addr)?;

        let mut value: u16 = 0;
        for (i, _) in ((bits.start() / 8)..=(bits.end() / 8)).enumerate() {
            self.phy.cmd(0x9007)?; // Read with post-increment

            // Read the actual byte from the config vuffer
            let r = self.phy.read(phy::GPIO::MICRO_PAGE())?;
            let r = (u16::from(r) >> 4) as u8;

            // Accumulate into a u16
            value |= u16::from(r) << (i * 8);
        }
        let bit_start = bits.start() % 8;
        let bit_size: u16 = bits.end() - bits.start() + 1;
        let mask: u8 = (1u8 << bit_size).wrapping_sub(1);
        Ok((value >> bit_start) as u8 & mask)
    }
}

#[derive(Copy, Clone, IntoBytes, FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct TeslaSerdes6gPatch {
    cfg: [u8; 38],
    // There's also a status buf, but we'll skip that for now
}

#[derive(Copy, Clone, IntoBytes, FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct TeslaSerdes6gObConfig {
    pub ob_post0: u8,
    pub ob_post1: u8,
    pub ob_prec: u8,
    pub ob_sr_h: u8,
    pub ob_sr: u8,
}

impl TeslaSerdes6gObConfig {
    /// Check that the arguments are within the correct range
    fn check_range(&self) -> Result<(), VscError> {
        if self.ob_post0 > 63
            || self.ob_post1 > 31
            || self.ob_prec > 31
            || self.ob_sr > 15
            || self.ob_sr_h > 1
        {
            Err(VscError::OutOfRange)
        } else {
            Ok(())
        }
    }
}

const TESLA_TR_CONFIG: [((u16, u8), u16); 181] = [
    (detype(phy::TR::TR_18()), 0x0004),
    (detype(phy::TR::TR_17()), 0x01bd),
    (detype(phy::TR::TR_16()), 0x8fae),
    (detype(phy::TR::TR_18()), 0x000f),
    (detype(phy::TR::TR_17()), 0x000f),
    (detype(phy::TR::TR_16()), 0x8fac),
    (detype(phy::TR::TR_18()), 0x00a0),
    (detype(phy::TR::TR_17()), 0xf147),
    (detype(phy::TR::TR_16()), 0x97a0),
    (detype(phy::TR::TR_18()), 0x0005),
    (detype(phy::TR::TR_17()), 0x2f54),
    (detype(phy::TR::TR_16()), 0x8fe4),
    (detype(phy::TR::TR_18()), 0x0027),
    (detype(phy::TR::TR_17()), 0x303d),
    (detype(phy::TR::TR_16()), 0x9792),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0704),
    (detype(phy::TR::TR_16()), 0x87fe),
    (detype(phy::TR::TR_18()), 0x0006),
    (detype(phy::TR::TR_17()), 0x0150),
    (detype(phy::TR::TR_16()), 0x8fe0),
    (detype(phy::TR::TR_18()), 0x0012),
    (detype(phy::TR::TR_17()), 0xb00a),
    (detype(phy::TR::TR_16()), 0x8f82),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0d74),
    (detype(phy::TR::TR_16()), 0x8f80),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0012),
    (detype(phy::TR::TR_16()), 0x82e0),
    (detype(phy::TR::TR_18()), 0x0005),
    (detype(phy::TR::TR_17()), 0x0208),
    (detype(phy::TR::TR_16()), 0x83a2),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x9186),
    (detype(phy::TR::TR_16()), 0x83b2),
    (detype(phy::TR::TR_18()), 0x000e),
    (detype(phy::TR::TR_17()), 0x3700),
    (detype(phy::TR::TR_16()), 0x8fb0),
    (detype(phy::TR::TR_18()), 0x0004),
    (detype(phy::TR::TR_17()), 0x9f81),
    (detype(phy::TR::TR_16()), 0x9688),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0xffff),
    (detype(phy::TR::TR_16()), 0x8fd2),
    (detype(phy::TR::TR_18()), 0x0003),
    (detype(phy::TR::TR_17()), 0x9fa2),
    (detype(phy::TR::TR_16()), 0x968a),
    (detype(phy::TR::TR_18()), 0x0020),
    (detype(phy::TR::TR_17()), 0x640b),
    (detype(phy::TR::TR_16()), 0x9690),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x2220),
    (detype(phy::TR::TR_16()), 0x8258),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x2a20),
    (detype(phy::TR::TR_16()), 0x825a),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x3060),
    (detype(phy::TR::TR_16()), 0x825c),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x3fa0),
    (detype(phy::TR::TR_16()), 0x825e),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0xe0f0),
    (detype(phy::TR::TR_16()), 0x83a6),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x1489),
    (detype(phy::TR::TR_16()), 0x8f92),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x7000),
    (detype(phy::TR::TR_16()), 0x96a2),
    (detype(phy::TR::TR_18()), 0x0007),
    (detype(phy::TR::TR_17()), 0x1448),
    (detype(phy::TR::TR_16()), 0x96a6),
    (detype(phy::TR::TR_18()), 0x00ee),
    (detype(phy::TR::TR_17()), 0xffdd),
    (detype(phy::TR::TR_16()), 0x96a0),
    (detype(phy::TR::TR_18()), 0x0091),
    (detype(phy::TR::TR_17()), 0xb06c),
    (detype(phy::TR::TR_16()), 0x8fe8),
    (detype(phy::TR::TR_18()), 0x0004),
    (detype(phy::TR::TR_17()), 0x1600),
    (detype(phy::TR::TR_16()), 0x8fea),
    (detype(phy::TR::TR_18()), 0x00ee),
    (detype(phy::TR::TR_17()), 0xff00),
    (detype(phy::TR::TR_16()), 0x96b0),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x7000),
    (detype(phy::TR::TR_16()), 0x96b2),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0814),
    (detype(phy::TR::TR_16()), 0x96b4),
    (detype(phy::TR::TR_18()), 0x0068),
    (detype(phy::TR::TR_17()), 0x8980),
    (detype(phy::TR::TR_16()), 0x8f90),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0xd8f0),
    (detype(phy::TR::TR_16()), 0x83a4),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0400),
    (detype(phy::TR::TR_16()), 0x8fc0),
    (detype(phy::TR::TR_18()), 0x0050),
    (detype(phy::TR::TR_17()), 0x100f),
    (detype(phy::TR::TR_16()), 0x87fa),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0003),
    (detype(phy::TR::TR_16()), 0x8796),
    (detype(phy::TR::TR_18()), 0x00c3),
    (detype(phy::TR::TR_17()), 0xff98),
    (detype(phy::TR::TR_16()), 0x87f8),
    (detype(phy::TR::TR_18()), 0x0018),
    (detype(phy::TR::TR_17()), 0x292a),
    (detype(phy::TR::TR_16()), 0x8fa4),
    (detype(phy::TR::TR_18()), 0x00d2),
    (detype(phy::TR::TR_17()), 0xc46f),
    (detype(phy::TR::TR_16()), 0x968c),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0620),
    (detype(phy::TR::TR_16()), 0x97a2),
    (detype(phy::TR::TR_18()), 0x0013),
    (detype(phy::TR::TR_17()), 0x132f),
    (detype(phy::TR::TR_16()), 0x96a4),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0000),
    (detype(phy::TR::TR_16()), 0x96a8),
    (detype(phy::TR::TR_18()), 0x00c0),
    (detype(phy::TR::TR_17()), 0xa028),
    (detype(phy::TR::TR_16()), 0x8ffc),
    (detype(phy::TR::TR_18()), 0x0090),
    (detype(phy::TR::TR_17()), 0x1c09),
    (detype(phy::TR::TR_16()), 0x8fec),
    (detype(phy::TR::TR_18()), 0x0004),
    (detype(phy::TR::TR_17()), 0xa6a1),
    (detype(phy::TR::TR_16()), 0x8fee),
    (detype(phy::TR::TR_18()), 0x00b0),
    (detype(phy::TR::TR_17()), 0x1807),
    (detype(phy::TR::TR_16()), 0x8ffe),
    // We're not using 10BASE-TE, so this is the correct config block
    (detype(phy::TR::TR_16()), 0x028e),
    (detype(phy::TR::TR_18()), 0x0008),
    (detype(phy::TR::TR_17()), 0xa518),
    (detype(phy::TR::TR_16()), 0x8486),
    (detype(phy::TR::TR_18()), 0x006d),
    (detype(phy::TR::TR_17()), 0xc696),
    (detype(phy::TR::TR_16()), 0x8488),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0912),
    (detype(phy::TR::TR_16()), 0x848a),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0db6),
    (detype(phy::TR::TR_16()), 0x848e),
    (detype(phy::TR::TR_18()), 0x0059),
    (detype(phy::TR::TR_17()), 0x6596),
    (detype(phy::TR::TR_16()), 0x849c),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0514),
    (detype(phy::TR::TR_16()), 0x849e),
    (detype(phy::TR::TR_18()), 0x0041),
    (detype(phy::TR::TR_17()), 0x0280),
    (detype(phy::TR::TR_16()), 0x84a2),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0000),
    (detype(phy::TR::TR_16()), 0x84a4),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0000),
    (detype(phy::TR::TR_16()), 0x84a6),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0000),
    (detype(phy::TR::TR_16()), 0x84a8),
    (detype(phy::TR::TR_18()), 0x0000),
    (detype(phy::TR::TR_17()), 0x0000),
    (detype(phy::TR::TR_16()), 0x84aa),
    (detype(phy::TR::TR_18()), 0x007d),
    (detype(phy::TR::TR_17()), 0xf7dd),
    (detype(phy::TR::TR_16()), 0x84ae),
    (detype(phy::TR::TR_18()), 0x006d),
    (detype(phy::TR::TR_17()), 0x95d4),
    (detype(phy::TR::TR_16()), 0x84b0),
    (detype(phy::TR::TR_18()), 0x0049),
    (detype(phy::TR::TR_17()), 0x2410),
    (detype(phy::TR::TR_16()), 0x84b2),
];

/// Raw patch for 8051 microcode, from `tesla_revB_8051_patch` in the SDK
const TESLA_PATCH: [u8; 1655] = [
    0x46, 0x4a, 0x02, 0x43, 0x37, 0x02, 0x46, 0x26, 0x02, 0x46, 0x77, 0x02,
    0x45, 0x60, 0x02, 0x45, 0xaf, 0xed, 0xff, 0xe5, 0xfc, 0x54, 0x38, 0x64,
    0x20, 0x70, 0x08, 0x65, 0xff, 0x70, 0x04, 0xed, 0x44, 0x80, 0xff, 0x22,
    0x8f, 0x19, 0x7b, 0xbb, 0x7d, 0x0e, 0x7f, 0x04, 0x12, 0x3d, 0xd7, 0xef,
    0x4e, 0x60, 0x03, 0x02, 0x41, 0xf9, 0xe4, 0xf5, 0x1a, 0x74, 0x01, 0x7e,
    0x00, 0xa8, 0x1a, 0x08, 0x80, 0x05, 0xc3, 0x33, 0xce, 0x33, 0xce, 0xd8,
    0xf9, 0xff, 0xef, 0x55, 0x19, 0x70, 0x03, 0x02, 0x41, 0xed, 0x85, 0x1a,
    0xfb, 0x7b, 0xbb, 0xe4, 0xfd, 0xff, 0x12, 0x3d, 0xd7, 0xef, 0x4e, 0x60,
    0x03, 0x02, 0x41, 0xed, 0xe5, 0x1a, 0x54, 0x02, 0x75, 0x1d, 0x00, 0x25,
    0xe0, 0x25, 0xe0, 0xf5, 0x1c, 0xe4, 0x78, 0xc5, 0xf6, 0xd2, 0x0a, 0x12,
    0x41, 0xfa, 0x7b, 0xff, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3d, 0xd7, 0xef,
    0x4e, 0x60, 0x03, 0x02, 0x41, 0xe7, 0xc2, 0x0a, 0x74, 0xc7, 0x25, 0x1a,
    0xf9, 0x74, 0xe7, 0x25, 0x1a, 0xf8, 0xe6, 0x27, 0xf5, 0x1b, 0xe5, 0x1d,
    0x24, 0x5b, 0x12, 0x45, 0xea, 0x12, 0x3e, 0xda, 0x7b, 0xfc, 0x7d, 0x11,
    0x7f, 0x07, 0x12, 0x3d, 0xd7, 0x78, 0xcc, 0xef, 0xf6, 0x78, 0xc1, 0xe6,
    0xfe, 0xef, 0xd3, 0x9e, 0x40, 0x06, 0x78, 0xcc, 0xe6, 0x78, 0xc1, 0xf6,
    0x12, 0x41, 0xfa, 0x7b, 0xec, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3d, 0xd7,
    0x78, 0xcb, 0xef, 0xf6, 0xbf, 0x07, 0x06, 0x78, 0xc3, 0x76, 0x1a, 0x80,
    0x1f, 0x78, 0xc5, 0xe6, 0xff, 0x60, 0x0f, 0xc3, 0xe5, 0x1b, 0x9f, 0xff,
    0x78, 0xcb, 0xe6, 0x85, 0x1b, 0xf0, 0xa4, 0x2f, 0x80, 0x07, 0x78, 0xcb,
    0xe6, 0x85, 0x1b, 0xf0, 0xa4, 0x78, 0xc3, 0xf6, 0xe4, 0x78, 0xc2, 0xf6,
    0x78, 0xc2, 0xe6, 0xff, 0xc3, 0x08, 0x96, 0x40, 0x03, 0x02, 0x41, 0xd1,
    0xef, 0x54, 0x03, 0x60, 0x33, 0x14, 0x60, 0x46, 0x24, 0xfe, 0x60, 0x42,
    0x04, 0x70, 0x4b, 0xef, 0x24, 0x02, 0xff, 0xe4, 0x33, 0xfe, 0xef, 0x78,
    0x02, 0xce, 0xa2, 0xe7, 0x13, 0xce, 0x13, 0xd8, 0xf8, 0xff, 0xe5, 0x1d,
    0x24, 0x5c, 0xcd, 0xe5, 0x1c, 0x34, 0xf0, 0xcd, 0x2f, 0xff, 0xed, 0x3e,
    0xfe, 0x12, 0x46, 0x0d, 0x7d, 0x11, 0x80, 0x0b, 0x78, 0xc2, 0xe6, 0x70,
    0x04, 0x7d, 0x11, 0x80, 0x02, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3e, 0x9a,
    0x8e, 0x1e, 0x8f, 0x1f, 0x80, 0x03, 0xe5, 0x1e, 0xff, 0x78, 0xc5, 0xe6,
    0x06, 0x24, 0xcd, 0xf8, 0xa6, 0x07, 0x78, 0xc2, 0x06, 0xe6, 0xb4, 0x1a,
    0x0a, 0xe5, 0x1d, 0x24, 0x5c, 0x12, 0x45, 0xea, 0x12, 0x3e, 0xda, 0x78,
    0xc5, 0xe6, 0x65, 0x1b, 0x70, 0x82, 0x75, 0xdb, 0x20, 0x75, 0xdb, 0x28,
    0x12, 0x46, 0x02, 0x12, 0x46, 0x02, 0xe5, 0x1a, 0x12, 0x45, 0xf5, 0xe5,
    0x1a, 0xc3, 0x13, 0x12, 0x45, 0xf5, 0x78, 0xc5, 0x16, 0xe6, 0x24, 0xcd,
    0xf8, 0xe6, 0xff, 0x7e, 0x08, 0x1e, 0xef, 0xa8, 0x06, 0x08, 0x80, 0x02,
    0xc3, 0x13, 0xd8, 0xfc, 0xfd, 0xc4, 0x33, 0x54, 0xe0, 0xf5, 0xdb, 0xef,
    0xa8, 0x06, 0x08, 0x80, 0x02, 0xc3, 0x13, 0xd8, 0xfc, 0xfd, 0xc4, 0x33,
    0x54, 0xe0, 0x44, 0x08, 0xf5, 0xdb, 0xee, 0x70, 0xd8, 0x78, 0xc5, 0xe6,
    0x70, 0xc8, 0x75, 0xdb, 0x10, 0x02, 0x40, 0xfd, 0x78, 0xc2, 0xe6, 0xc3,
    0x94, 0x17, 0x50, 0x0e, 0xe5, 0x1d, 0x24, 0x62, 0x12, 0x42, 0x08, 0xe5,
    0x1d, 0x24, 0x5c, 0x12, 0x42, 0x08, 0x20, 0x0a, 0x03, 0x02, 0x40, 0x76,
    0x05, 0x1a, 0xe5, 0x1a, 0xc3, 0x94, 0x04, 0x50, 0x03, 0x02, 0x40, 0x3a,
    0x22, 0xe5, 0x1d, 0x24, 0x5c, 0xff, 0xe5, 0x1c, 0x34, 0xf0, 0xfe, 0x12,
    0x46, 0x0d, 0x22, 0xff, 0xe5, 0x1c, 0x34, 0xf0, 0xfe, 0x12, 0x46, 0x0d,
    0x22, 0xe4, 0xf5, 0x19, 0x12, 0x46, 0x43, 0x20, 0xe7, 0x1e, 0x7b, 0xfe,
    0x12, 0x42, 0xf9, 0xef, 0xc4, 0x33, 0x33, 0x54, 0xc0, 0xff, 0xc0, 0x07,
    0x7b, 0x54, 0x12, 0x42, 0xf9, 0xd0, 0xe0, 0x4f, 0xff, 0x74, 0x2a, 0x25,
    0x19, 0xf8, 0xa6, 0x07, 0x12, 0x46, 0x43, 0x20, 0xe7, 0x03, 0x02, 0x42,
    0xdf, 0x54, 0x03, 0x64, 0x03, 0x70, 0x03, 0x02, 0x42, 0xcf, 0x7b, 0xcb,
    0x12, 0x43, 0x2c, 0x8f, 0xfb, 0x7b, 0x30, 0x7d, 0x03, 0xe4, 0xff, 0x12,
    0x3d, 0xd7, 0xc3, 0xef, 0x94, 0x02, 0xee, 0x94, 0x00, 0x50, 0x2a, 0x12,
    0x42, 0xec, 0xef, 0x4e, 0x70, 0x23, 0x12, 0x43, 0x04, 0x60, 0x0a, 0x12,
    0x43, 0x12, 0x70, 0x0c, 0x12, 0x43, 0x1f, 0x70, 0x07, 0x12, 0x46, 0x39,
    0x7b, 0x03, 0x80, 0x07, 0x12, 0x46, 0x39, 0x12, 0x46, 0x43, 0xfb, 0x7a,
    0x00, 0x7d, 0x54, 0x80, 0x3e, 0x12, 0x42, 0xec, 0xef, 0x4e, 0x70, 0x24,
    0x12, 0x43, 0x04, 0x60, 0x0a, 0x12, 0x43, 0x12, 0x70, 0x0f, 0x12, 0x43,
    0x1f, 0x70, 0x0a, 0x12, 0x46, 0x39, 0xe4, 0xfb, 0xfa, 0x7d, 0xee, 0x80,
    0x1e, 0x12, 0x46, 0x39, 0x7b, 0x01, 0x7a, 0x00, 0x7d, 0xee, 0x80, 0x13,
    0x12, 0x46, 0x39, 0x12, 0x46, 0x43, 0x54, 0x40, 0xfe, 0xc4, 0x13, 0x13,
    0x54, 0x03, 0xfb, 0x7a, 0x00, 0x7d, 0xee, 0x12, 0x38, 0xbd, 0x7b, 0xff,
    0x12, 0x43, 0x2c, 0xef, 0x4e, 0x70, 0x07, 0x74, 0x2a, 0x25, 0x19, 0xf8,
    0xe4, 0xf6, 0x05, 0x19, 0xe5, 0x19, 0xc3, 0x94, 0x02, 0x50, 0x03, 0x02,
    0x42, 0x15, 0x22, 0xe5, 0x19, 0x24, 0x17, 0xfd, 0x7b, 0x20, 0x7f, 0x04,
    0x12, 0x3d, 0xd7, 0x22, 0xe5, 0x19, 0x24, 0x17, 0xfd, 0x7f, 0x04, 0x12,
    0x3d, 0xd7, 0x22, 0x7b, 0x22, 0x7d, 0x18, 0x7f, 0x06, 0x12, 0x3d, 0xd7,
    0xef, 0x64, 0x01, 0x4e, 0x22, 0x7d, 0x1c, 0xe4, 0xff, 0x12, 0x3e, 0x9a,
    0xef, 0x54, 0x1b, 0x64, 0x0a, 0x22, 0x7b, 0xcc, 0x7d, 0x10, 0xff, 0x12,
    0x3d, 0xd7, 0xef, 0x64, 0x01, 0x4e, 0x22, 0xe5, 0x19, 0x24, 0x17, 0xfd,
    0x7f, 0x04, 0x12, 0x3d, 0xd7, 0x22, 0xd2, 0x08, 0x75, 0xfb, 0x03, 0xab,
    0x7e, 0xaa, 0x7d, 0x7d, 0x19, 0x7f, 0x03, 0x12, 0x3e, 0xda, 0xe5, 0x7e,
    0x54, 0x0f, 0x24, 0xf3, 0x60, 0x03, 0x02, 0x43, 0xe9, 0x12, 0x46, 0x5a,
    0x12, 0x46, 0x61, 0xd8, 0xfb, 0xff, 0x20, 0xe2, 0x35, 0x13, 0x92, 0x0c,
    0xef, 0xa2, 0xe1, 0x92, 0x0b, 0x30, 0x0c, 0x2a, 0xe4, 0xf5, 0x10, 0x7b,
    0xfe, 0x12, 0x43, 0xff, 0xef, 0xc4, 0x33, 0x33, 0x54, 0xc0, 0xff, 0xc0,
    0x07, 0x7b, 0x54, 0x12, 0x43, 0xff, 0xd0, 0xe0, 0x4f, 0xff, 0x74, 0x2a,
    0x25, 0x10, 0xf8, 0xa6, 0x07, 0x05, 0x10, 0xe5, 0x10, 0xc3, 0x94, 0x02,
    0x40, 0xd9, 0x12, 0x46, 0x5a, 0x12, 0x46, 0x61, 0xd8, 0xfb, 0x54, 0x05,
    0x64, 0x04, 0x70, 0x27, 0x78, 0xc4, 0xe6, 0x78, 0xc6, 0xf6, 0xe5, 0x7d,
    0xff, 0x33, 0x95, 0xe0, 0xef, 0x54, 0x0f, 0x78, 0xc4, 0xf6, 0x12, 0x44,
    0x0a, 0x20, 0x0c, 0x0c, 0x12, 0x46, 0x5a, 0x12, 0x46, 0x61, 0xd8, 0xfb,
    0x13, 0x92, 0x0d, 0x22, 0xc2, 0x0d, 0x22, 0x12, 0x46, 0x5a, 0x12, 0x46,
    0x61, 0xd8, 0xfb, 0x54, 0x05, 0x64, 0x05, 0x70, 0x1e, 0x78, 0xc4, 0x7d,
    0xb8, 0x12, 0x43, 0xf5, 0x78, 0xc1, 0x7d, 0x74, 0x12, 0x43, 0xf5, 0xe4,
    0x78, 0xc1, 0xf6, 0x22, 0x7b, 0x01, 0x7a, 0x00, 0x7d, 0xee, 0x7f, 0x92,
    0x12, 0x38, 0xbd, 0x22, 0xe6, 0xfb, 0x7a, 0x00, 0x7f, 0x92, 0x12, 0x38,
    0xbd, 0x22, 0xe5, 0x10, 0x24, 0x17, 0xfd, 0x7f, 0x04, 0x12, 0x3d, 0xd7,
    0x22, 0x78, 0xc1, 0xe6, 0xfb, 0x7a, 0x00, 0x7d, 0x74, 0x7f, 0x92, 0x12,
    0x38, 0xbd, 0xe4, 0x78, 0xc1, 0xf6, 0xf5, 0x11, 0x74, 0x01, 0x7e, 0x00,
    0xa8, 0x11, 0x08, 0x80, 0x05, 0xc3, 0x33, 0xce, 0x33, 0xce, 0xd8, 0xf9,
    0xff, 0x78, 0xc4, 0xe6, 0xfd, 0xef, 0x5d, 0x60, 0x44, 0x85, 0x11, 0xfb,
    0xe5, 0x11, 0x54, 0x02, 0x25, 0xe0, 0x25, 0xe0, 0xfe, 0xe4, 0x24, 0x5b,
    0xfb, 0xee, 0x12, 0x45, 0xed, 0x12, 0x3e, 0xda, 0x7b, 0x40, 0x7d, 0x11,
    0x7f, 0x07, 0x12, 0x3d, 0xd7, 0x74, 0xc7, 0x25, 0x11, 0xf8, 0xa6, 0x07,
    0x7b, 0x11, 0x7d, 0x12, 0x7f, 0x07, 0x12, 0x3d, 0xd7, 0xef, 0x4e, 0x60,
    0x09, 0x74, 0xe7, 0x25, 0x11, 0xf8, 0x76, 0x04, 0x80, 0x07, 0x74, 0xe7,
    0x25, 0x11, 0xf8, 0x76, 0x0a, 0x05, 0x11, 0xe5, 0x11, 0xc3, 0x94, 0x04,
    0x40, 0x9a, 0x78, 0xc6, 0xe6, 0x70, 0x15, 0x78, 0xc4, 0xe6, 0x60, 0x10,
    0x75, 0xd9, 0x38, 0x75, 0xdb, 0x10, 0x7d, 0xfe, 0x12, 0x44, 0xb8, 0x7d,
    0x76, 0x12, 0x44, 0xb8, 0x79, 0xc6, 0xe7, 0x78, 0xc4, 0x66, 0xff, 0x60,
    0x03, 0x12, 0x40, 0x25, 0x78, 0xc4, 0xe6, 0x70, 0x09, 0xfb, 0xfa, 0x7d,
    0xfe, 0x7f, 0x8e, 0x12, 0x38, 0xbd, 0x22, 0x7b, 0x01, 0x7a, 0x00, 0x7f,
    0x8e, 0x12, 0x38, 0xbd, 0x22, 0xe4, 0xf5, 0xfb, 0x7d, 0x1c, 0xe4, 0xff,
    0x12, 0x3e, 0x9a, 0xad, 0x07, 0xac, 0x06, 0xec, 0x54, 0xc0, 0xff, 0xed,
    0x54, 0x3f, 0x4f, 0xf5, 0x20, 0x30, 0x06, 0x2c, 0x30, 0x01, 0x08, 0xa2,
    0x04, 0x72, 0x03, 0x92, 0x07, 0x80, 0x21, 0x30, 0x04, 0x06, 0x7b, 0xcc,
    0x7d, 0x11, 0x80, 0x0d, 0x30, 0x03, 0x06, 0x7b, 0xcc, 0x7d, 0x10, 0x80,
    0x04, 0x7b, 0x66, 0x7d, 0x16, 0xe4, 0xff, 0x12, 0x3d, 0xd7, 0xee, 0x4f,
    0x24, 0xff, 0x92, 0x07, 0xaf, 0xfb, 0x74, 0x26, 0x2f, 0xf8, 0xe6, 0xff,
    0xa6, 0x20, 0x20, 0x07, 0x39, 0x8f, 0x20, 0x30, 0x07, 0x34, 0x30, 0x00,
    0x31, 0x20, 0x04, 0x2e, 0x20, 0x03, 0x2b, 0xe4, 0xf5, 0xff, 0x75, 0xfc,
    0xc2, 0xe5, 0xfc, 0x30, 0xe0, 0xfb, 0xaf, 0xfe, 0xef, 0x20, 0xe3, 0x1a,
    0xae, 0xfd, 0x44, 0x08, 0xf5, 0xfe, 0x75, 0xfc, 0x80, 0xe5, 0xfc, 0x30,
    0xe0, 0xfb, 0x8f, 0xfe, 0x8e, 0xfd, 0x75, 0xfc, 0x80, 0xe5, 0xfc, 0x30,
    0xe0, 0xfb, 0x05, 0xfb, 0xaf, 0xfb, 0xef, 0xc3, 0x94, 0x04, 0x50, 0x03,
    0x02, 0x44, 0xc5, 0xe4, 0xf5, 0xfb, 0x22, 0xe5, 0x7e, 0x54, 0x0f, 0x64,
    0x01, 0x70, 0x23, 0xe5, 0x7e, 0x30, 0xe4, 0x1e, 0x90, 0x47, 0xd0, 0xe0,
    0x44, 0x02, 0xf0, 0x54, 0xfb, 0xf0, 0x90, 0x47, 0xd4, 0xe0, 0x44, 0x04,
    0xf0, 0x7b, 0x03, 0x7d, 0x5b, 0x7f, 0x5d, 0x12, 0x36, 0x29, 0x7b, 0x0e,
    0x80, 0x1c, 0x90, 0x47, 0xd0, 0xe0, 0x54, 0xfd, 0xf0, 0x44, 0x04, 0xf0,
    0x90, 0x47, 0xd4, 0xe0, 0x54, 0xfb, 0xf0, 0x7b, 0x02, 0x7d, 0x5b, 0x7f,
    0x5d, 0x12, 0x36, 0x29, 0x7b, 0x06, 0x7d, 0x60, 0x7f, 0x63, 0x12, 0x36,
    0x29, 0x22, 0xe5, 0x7e, 0x30, 0xe5, 0x35, 0x30, 0xe4, 0x0b, 0x7b, 0x02,
    0x7d, 0x33, 0x7f, 0x35, 0x12, 0x36, 0x29, 0x80, 0x10, 0x7b, 0x01, 0x7d,
    0x33, 0x7f, 0x35, 0x12, 0x36, 0x29, 0x90, 0x47, 0xd2, 0xe0, 0x44, 0x04,
    0xf0, 0x90, 0x47, 0xd2, 0xe0, 0x54, 0xf7, 0xf0, 0x90, 0x47, 0xd1, 0xe0,
    0x44, 0x10, 0xf0, 0x7b, 0x05, 0x7d, 0x84, 0x7f, 0x86, 0x12, 0x36, 0x29,
    0x22, 0xfb, 0xe5, 0x1c, 0x34, 0xf0, 0xfa, 0x7d, 0x10, 0x7f, 0x07, 0x22,
    0x54, 0x01, 0xc4, 0x33, 0x54, 0xe0, 0xf5, 0xdb, 0x44, 0x08, 0xf5, 0xdb,
    0x22, 0xf5, 0xdb, 0x75, 0xdb, 0x08, 0xf5, 0xdb, 0x75, 0xdb, 0x08, 0x22,
    0xab, 0x07, 0xaa, 0x06, 0x7d, 0x10, 0x7f, 0x07, 0x12, 0x3e, 0xda, 0x7b,
    0xff, 0x7d, 0x10, 0x7f, 0x07, 0x12, 0x3d, 0xd7, 0xef, 0x4e, 0x60, 0xf3,
    0x22, 0x12, 0x44, 0xc2, 0x30, 0x0c, 0x03, 0x12, 0x42, 0x12, 0x78, 0xc4,
    0xe6, 0xff, 0x60, 0x03, 0x12, 0x40, 0x25, 0x22, 0xe5, 0x19, 0x24, 0x17,
    0x54, 0x1f, 0x44, 0x80, 0xff, 0x22, 0x74, 0x2a, 0x25, 0x19, 0xf8, 0xe6,
    0x22, 0x12, 0x46, 0x72, 0x12, 0x46, 0x68, 0x90, 0x47, 0xfa, 0xe0, 0x54,
    0xf8, 0x44, 0x02, 0xf0, 0x22, 0xe5, 0x7e, 0xae, 0x7d, 0x78, 0x04, 0x22,
    0xce, 0xa2, 0xe7, 0x13, 0xce, 0x13, 0x22, 0xe4, 0x78, 0xc4, 0xf6, 0xc2,
    0x0d, 0x78, 0xc1, 0xf6, 0x22, 0xc2, 0x0c, 0xc2, 0x0b, 0x22, 0x22,
];
