// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::Trace;
use crate::{Phy, PhyRw};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use vsc7448_pac::phy;
use vsc_err::VscError;

pub struct ViperPhy<'a, 'b, P> {
    pub phy: &'b mut Phy<'a, P>,
}

impl<'a, 'b, P: PhyRw> ViperPhy<'a, 'b, P> {
    /// Applies a patch to the 8051 microcode inside the PHY, based on
    /// `vtss_phy_pre_init_seq_viper` in the SDK, which calls
    /// `vtss_phy_pre_init_seq_viper_rev_b`
    pub(crate) fn patch(&mut self) -> Result<(), VscError> {
        ringbuf_entry!(Trace::ViperPatch(self.phy.port));

        self.phy
            .modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
                *r = (u16::from(*r) | 1).into()
            })?;
        self.phy.modify(phy::STANDARD::BYPASS_CONTROL(), |r| {
            *r = (u16::from(*r) | 8).into()
        })?;
        self.phy.write(
            phy::EXTENDED_3::MEDIA_SERDES_TX_CRC_ERROR_COUNTER(),
            0x2000.into(),
        )?;
        self.phy.write(phy::TEST::TEST_PAGE_5(), 0x1f20.into())?;
        self.phy
            .modify(phy::TEST::TEST_PAGE_8(), |r| r.0 |= 0x8000)?;
        self.phy.write(phy::TR::TR_16(), 0xafa4.into())?;
        self.phy
            .modify(phy::TR::TR_18(), |r| r.0 = (r.0 & !0x7f) | 0x19)?;

        self.phy.write(phy::TR::TR_16(), 0x8fa4.into())?;
        self.phy.write(phy::TR::TR_18(), 0x0050.into())?;
        self.phy.write(phy::TR::TR_17(), 0x100f.into())?;
        self.phy.write(phy::TR::TR_16(), 0x87fa.into())?;
        self.phy.write(phy::TR::TR_18(), 0x0004.into())?;
        self.phy.write(phy::TR::TR_17(), 0x9f81.into())?;
        self.phy.write(phy::TR::TR_16(), 0x9688.into())?;

        // "Init script updates from James Bz#22267"
        self.phy.write(phy::TR::TR_18(), 0x0068.into())?;
        self.phy.write(phy::TR::TR_17(), 0x8980.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8f90.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0xd8f0.into())?;
        self.phy.write(phy::TR::TR_16(), 0x83a4.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0x0400.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8fc0.into())?;

        // "EEE updates from James Bz#22267"
        self.phy.write(phy::TR::TR_18(), 0x0012.into())?;
        self.phy.write(phy::TR::TR_17(), 0xb002.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8f82.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0x0004.into())?;
        self.phy.write(phy::TR::TR_16(), 0x9686.into())?;

        self.phy.write(phy::TR::TR_18(), 0x00d2.into())?;
        self.phy.write(phy::TR::TR_17(), 0xc46f.into())?;
        self.phy.write(phy::TR::TR_16(), 0x968c.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0x0620.into())?;
        self.phy.write(phy::TR::TR_16(), 0x97a2.into())?;

        self.phy.write(phy::TR::TR_18(), 0x00ee.into())?;
        self.phy.write(phy::TR::TR_17(), 0xffdd.into())?;
        self.phy.write(phy::TR::TR_16(), 0x96a0.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0007.into())?;
        self.phy.write(phy::TR::TR_17(), 0x1448.into())?;
        self.phy.write(phy::TR::TR_16(), 0x96a6.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0013.into())?;
        self.phy.write(phy::TR::TR_17(), 0x132f.into())?;
        self.phy.write(phy::TR::TR_16(), 0x96a4.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_16(), 0x96a8.into())?;

        self.phy.write(phy::TR::TR_18(), 0x00c0.into())?;
        self.phy.write(phy::TR::TR_17(), 0xa028.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8ffc.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0091.into())?;
        self.phy.write(phy::TR::TR_17(), 0xb06c.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8fe8.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0004.into())?;
        self.phy.write(phy::TR::TR_17(), 0x1600.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8fea.into())?;

        self.phy.write(phy::TR::TR_18(), 0x00ff.into())?;
        self.phy.write(phy::TR::TR_17(), 0xfaff.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8f80.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0090.into())?;
        self.phy.write(phy::TR::TR_17(), 0x1809.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8fec.into())?;

        self.phy.write(phy::TR::TR_18(), 0x00b0.into())?;
        self.phy.write(phy::TR::TR_17(), 0x1007.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8ffe.into())?;

        self.phy.write(phy::TR::TR_18(), 0x00ee.into())?;
        self.phy.write(phy::TR::TR_17(), 0xff00.into())?;
        self.phy.write(phy::TR::TR_16(), 0x96b0.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0x7000.into())?;
        self.phy.write(phy::TR::TR_16(), 0x96b2.into())?;

        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0x0814.into())?;
        self.phy.write(phy::TR::TR_16(), 0x96b4.into())?;

        // We aren't using 10Base-TE, so this is correct config block
        self.phy
            .write(phy::EXTENDED_2::CU_PMD_TX_CTRL(), 0x028e.into())?;
        self.phy.write(phy::TR::TR_18(), 0x0008.into())?;
        self.phy.write(phy::TR::TR_17(), 0xa518.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8486.into())?;
        self.phy.write(phy::TR::TR_18(), 0x006d.into())?;
        self.phy.write(phy::TR::TR_17(), 0xc696.into())?;
        self.phy.write(phy::TR::TR_16(), 0x8488.into())?;
        self.phy.write(phy::TR::TR_18(), 0x0000.into())?;
        self.phy.write(phy::TR::TR_17(), 0x0912.into())?;
        self.phy.write(phy::TR::TR_16(), 0x848a.into())?;

        self.phy.modify(phy::TEST::TEST_PAGE_8(), |r| {
            r.0 &= !0x8000;
        })?;
        self.phy
            .modify(phy::STANDARD::EXTENDED_CONTROL_AND_STATUS(), |r| {
                *r = (u16::from(*r) & !1).into();
            })?;

        //////////////////////////////////////////////////////////////////////
        // Now, we do the fun part of patching the 8051 PHY, based on
        // `viper_revB_8051_patch` in the SDK

        const FIRMWARE_START_ADDR: u16 = 0xE800;
        const PATCH_CRC_LEN: u16 = (VIPER_PATCH.len() + 1) as u16;
        const EXPECTED_CRC: u16 = 0xFB48;
        // This patch can only be applied to Port 0 of the PHY, so we'll check
        // the address here.
        let phy_port =
            self.phy.read(phy::EXTENDED::EXTENDED_PHY_CONTROL_4())?.0 >> 11;
        if phy_port != 0 {
            return Err(VscError::BadPhyPatchPort(phy_port));
        }

        let crc = self.phy.read_8051_crc(FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
        if crc == EXPECTED_CRC {
            return Ok(());
        }

        self.phy.download_patch(&VIPER_PATCH)?;
        // These writes only happen if vtss_state->syn_calling_private is
        // false, which seems like the default state?
        self.phy.write(phy::GPIO::GPIO_0(), 0x4018.into())?;
        self.phy.write(phy::GPIO::GPIO_0(), 0xc018.into())?;

        // Reread the CRC to make sure the download succeeded
        let crc = self.phy.read_8051_crc(FIRMWARE_START_ADDR, PATCH_CRC_LEN)?;
        if crc != EXPECTED_CRC {
            return Err(VscError::PhyPatchFailedCrc);
        }

        self.phy.micro_assert_reset()?;

        // "Clear all patches"
        self.phy.write(phy::GPIO::GPIO_12(), 0.into())?;

        // "Enable 8051 clock; set patch present; disable PRAM clock override
        //  and addr. auto-incr; operate at 125 MHz"
        self.phy.write(phy::GPIO::GPIO_0(), 0x4098.into())?;

        // "Release 8051 SW Reset"
        self.phy.write(phy::GPIO::GPIO_0(), 0xc098.into())?;

        // I'm not sure if these writes to GPIO_0 are superfluous, because we
        // also wrote to it above right after download_patch was called.
        Ok(())
    }
}

/// Raw patch for 8051 microcode, from `viper_revB_8051_patch` in the SDK
const VIPER_PATCH: [u8; 92] = [
    0xe8, 0x59, 0x02, 0xe8, 0x12, 0x02, 0xe8, 0x42, 0x02, 0xe8, 0x5a, 0x02,
    0xe8, 0x5b, 0x02, 0xe8, 0x5c, 0xe5, 0x69, 0x54, 0x0f, 0x24, 0xf7, 0x60,
    0x27, 0x24, 0xfc, 0x60, 0x23, 0x24, 0x08, 0x70, 0x14, 0xe5, 0x69, 0xae,
    0x68, 0x78, 0x04, 0xce, 0xa2, 0xe7, 0x13, 0xce, 0x13, 0xd8, 0xf8, 0x7e,
    0x00, 0x54, 0x0f, 0x80, 0x00, 0x7b, 0x01, 0x7a, 0x00, 0x7d, 0xee, 0x7f,
    0x92, 0x12, 0x50, 0xee, 0x22, 0xe4, 0xf5, 0x10, 0x85, 0x10, 0xfb, 0x7d,
    0x1c, 0xe4, 0xff, 0x12, 0x59, 0xea, 0x05, 0x10, 0xe5, 0x10, 0xc3, 0x94,
    0x04, 0x40, 0xed, 0x22, 0x22, 0x22, 0x22, 0x22,
];
