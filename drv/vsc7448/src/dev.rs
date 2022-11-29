// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{
    config::Speed,
    port::{port10g_flush, port1g_flush},
    Vsc7448Rw, VscError,
};
use vsc7448_pac::*;

/// DEV1G and DEV2G5 share the same register layout, so we can write functions
/// that use either one.
#[derive(Copy, Clone)]
pub enum DevGeneric {
    Dev1g(u8),
    Dev2g5(u8),
}

impl DevGeneric {
    /// Constructs a handle to a DEV1G device.  Returns an error on `d >= 24`,
    /// as there are only 24 DEV1G devices in the chip (numbering from 0).
    pub fn new_1g(d: u8) -> Result<Self, VscError> {
        if d < 24 {
            Ok(DevGeneric::Dev1g(d))
        } else {
            Err(VscError::InvalidDev1g(d))
        }
    }
    /// Constructs a handle to DEV2G5 device.  Returns an error on `d >= 29`,
    /// as there are only 29 DEV2G5 devices in the chip (numbering from 0).
    pub fn new_2g5(d: u8) -> Result<Self, VscError> {
        if d < 29 {
            Ok(DevGeneric::Dev2g5(d))
        } else {
            Err(VscError::InvalidDev2g5(d))
        }
    }
    /// Convert from a DEV to a port number.  Note that port numbers are
    /// not uniquely mapped to devices in the chip; it depends on how the
    /// chip is configured.
    pub fn port(&self) -> u8 {
        match *self {
            DevGeneric::Dev1g(d) => match d {
                0..=7 => d,
                // DEV1G_8-23 are only available in QSGMII mode, where
                // they map to ports 32-47 (Table 8)
                d => d + 24,
            },
            DevGeneric::Dev2g5(d) => match d {
                0..=23 => d + 8,
                // DEV2G5_24 is the NPI port, configured through SERDES1G_0
                24 => 48,
                // DEV2G5_25-28 are only available when running through
                // a SERDES10G in SGMII 1G/2.5G mode.  They map to ports
                // 49-52, using SERDES10G_0-3 (Table 9)
                d => d + 24,
            },
        }
    }
    /// Returns the register block for this device.  This is always a DEV1G
    /// address, because the layout is identical between DEV1G and DEV2G5, so
    /// this avoids duplication.
    pub fn regs(&self) -> vsc7448_pac::tgt::DEV1G {
        match *self {
            DevGeneric::Dev1g(d) => DEV1G(d),
            DevGeneric::Dev2g5(d) =>
            // We know that d is < 29 based on the check in the constructor.
            // DEV1G and DEV2G5 register blocks are identical in layout and
            // tightly packed, and there are 28 DEV2G5 register blocks, so
            // this should be a safe trick.
            {
                vsc7448_pac::tgt::DEV1G::from_raw_unchecked_address(
                    vsc7448_pac::tgt::DEV2G5::BASE
                        + u32::from(d) * vsc7448_pac::tgt::DEV1G::SIZE,
                )
            }
        }
    }

    /// Based on `jr2_port_conf_1g_set` in the SDK
    pub fn init_sgmii(
        &self,
        v: &impl Vsc7448Rw,
        speed: Speed,
    ) -> Result<(), VscError> {
        // In some cases, 2G5 ports shadow 10G ports.  If that's happening here,
        // then the caller must flush the 10G port separately before calling
        // this function, which only flushes the 1G port.
        port1g_flush(self, v)?;

        // Enable full duplex mode and GIGA SPEED
        let dev1g = self.regs();
        v.modify(dev1g.MAC_CFG_STATUS().MAC_MODE_CFG(), |r| {
            r.set_fdx_ena(1);
            r.set_giga_mode_ena(match speed {
                Speed::Speed1G => 1,
                Speed::Speed100M => 0,
                Speed::Speed10G => panic!("Invalid speed for SGMII"),
            });
        })?;

        v.modify(dev1g.MAC_CFG_STATUS().MAC_IFG_CFG(), |r| {
            match speed {
                // NOTE: these are speed-dependent options and aren't
                // fully documented in the manual; this values are chosen
                // based on the SDK.
                Speed::Speed1G => {
                    r.set_tx_ifg(4);
                    r.set_rx_ifg1(0);
                    r.set_rx_ifg2(0);
                }
                Speed::Speed100M => {
                    r.set_tx_ifg(6);
                    r.set_rx_ifg1(1);
                    r.set_rx_ifg2(4);
                }
                Speed::Speed10G => unreachable!(), // checked above
            }
        })?;

        // The upcoming steps depend on how the port is talking to the
        // outside world (100FX / SGMII / SERDES).  In this case, the port
        // is talking over QSGMII, which is configured like SGMII then
        // combined in the macro block (I may be butchering some details of
        // terminology or architecture here).

        // The device is configured to SGMII mode by default, so no
        // changes are needed there.

        // This bit isn't documented in the datasheet, but the SDK says it
        // must be set in SGMII mode.  It allows a link to be set up by
        // software, even if autonegotiation fails.
        v.write_with(dev1g.PCS1G_CFG_STATUS().PCS1G_ANEG_CFG(), |r| {
            // The SDK notes that we write the whole register here, instead of
            // just modifying one bit (since we're in CISCO SGMII mode)
            r.set_sw_resolve_ena(1);
        })?;

        // Configure signal detect line with values from the dev kit
        // This is dependent on the port setup.
        v.modify(dev1g.PCS1G_CFG_STATUS().PCS1G_SD_CFG(), |r| {
            r.set_sd_ena(0); // Ignored
        })?;

        // Enable the PCS!
        v.write_with(dev1g.PCS1G_CFG_STATUS().PCS1G_CFG(), |r| {
            r.set_pcs_ena(1);
        })?;

        v.modify(DSM().CFG().DEV_TX_STOP_WM_CFG(self.port()), |r| {
            r.set_dev_tx_stop_wm(match speed {
                // XXX In datasheet section 3.25.1, it says to set this to 3
                // instead, but the SDK always uses 0
                Speed::Speed1G => 0,
                Speed::Speed100M => 1,
                Speed::Speed10G => unreachable!(), // checked above
            })
        })?;

        // The SDK configures MAC VLAN awareness here; let's not do that
        // for the time being.

        // The SDK also configures flow control (`jr2_port_fc_setup`)
        // and policer flow control (`vtss_jr2_port_policer_fc_set`) around
        // here, which we'll skip.

        // Turn on the MAC!
        v.write_with(dev1g.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| {
            r.set_tx_ena(1);
            r.set_rx_ena(1);
        })?;

        // Take MAC, Port, Phy (intern), and PCS (SGMII) clocks out of
        // reset, turning on a 1G port data rate.
        v.write_with(dev1g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
            r.set_speed_sel(match speed {
                Speed::Speed1G => 2,
                Speed::Speed100M => 1,
                Speed::Speed10G => unreachable!(), // checked above
            });
        })?;

        v.modify(QFWD().SYSTEM().SWITCH_PORT_MODE(self.port()), |r| {
            r.set_port_ena(1);
            r.set_fwd_urgency(104); // This is different above 2.5G!
        })?;

        Ok(())
    }
}

/// Wrapper struct for a DEV10G index, which is analogous to `DevGeneric`.
/// The DEV10G target registers aren't identical to the DEV1G, so we need
/// to handle it differently.
#[derive(Copy, Clone)]
pub struct Dev10g(u8);
impl Dev10g {
    pub fn new(d: u8) -> Result<Self, VscError> {
        if d < 4 {
            Ok(Self(d))
        } else {
            Err(VscError::InvalidDev10g(d))
        }
    }
    /// Converts from a DEV10G index to a port index
    pub fn port(&self) -> u8 {
        self.0 + 49
    }
    pub fn regs(&self) -> vsc7448_pac::tgt::DEV10G {
        DEV10G(self.0)
    }
    pub fn index(&self) -> u8 {
        self.0
    }
    pub fn init_sfi(&self, v: &impl Vsc7448Rw) -> Result<(), VscError> {
        port10g_flush(self, v)?;

        // Remaining logic is from `jr2_port_conf_10g_set`
        // Handle signal detect
        let dev10g = self.regs();
        let pcs10g = PCS10G_BR(self.index());
        v.modify(pcs10g.PCS_10GBR_CFG().PCS_SD_CFG(), |r| {
            r.set_sd_ena(0);
        })?;
        // Enable SFI PCS
        v.modify(pcs10g.PCS_10GBR_CFG().PCS_CFG(), |r| {
            r.set_pcs_ena(1);
        })?;
        v.modify(dev10g.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| {
            r.set_rx_ena(1);
            r.set_tx_ena(1);
        })?;
        v.modify(dev10g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
            r.set_pcs_rx_rst(0);
            r.set_pcs_tx_rst(0);
            r.set_mac_rx_rst(0);
            r.set_mac_tx_rst(0);
            r.set_speed_sel(7); // SFI
        })?;
        v.modify(QFWD().SYSTEM().SWITCH_PORT_MODE(self.port()), |r| {
            r.set_port_ena(1);
            r.set_fwd_urgency(9);
        })?;

        Ok(())
    }
    pub fn init_10gbase_kr(&self, v: &impl Vsc7448Rw) -> Result<(), VscError> {
        // Based on `jr2_port_kr_conf_set` in the SDK
        let dev7 = XGKR1(self.index()); // ANEG
        let dev1 = XGKR0(self.index()); // Training
        let xfi = XGXFI(self.index()); // KR-Control/Stickies

        // "Adjust the timers for JR2 core clock (frequency of 250Mhz)
        v.write(dev7.LFLONG_TMR().LFLONG_MSW(), 322.into())?;
        v.write(dev7.TR_TMR().TR_MSW(), 322.into())?;
        v.modify(dev1.TR_CFG0().TR_CFG0(), |r| r.set_tmr_dvdr(6))?;
        v.write(dev1.WT_TMR().WT_TMR(), 1712.into())?;
        v.write(dev1.MW_TMR().MW_TMR_LSW(), 58521.into())?;
        v.write(dev1.MW_TMR().MW_TMR_MSW(), 204.into())?;

        // "Clear the KR_CONTROL stickies"
        v.write(xfi.XFI_CONTROL().KR_CONTROL(), 0x7FF.into())?;

        // "AN Selector"
        v.write(dev7.LD_ADV().KR_7X0010(), 0x0001.into())?;
        v.modify(dev7.LD_ADV().KR_7X0011(), |r| {
            r.set_adv1(1 << 7); // 10GBase-KR
        })?;
        v.modify(dev7.LD_ADV().KR_7X0012(), |r| {
            r.set_adv2(1 << 14); // FEC ANEG, but not requested (?)
        })?;
        v.modify(dev1.TR_CFG1().TR_CFG1(), |r| {
            r.set_tmr_hold(r.tmr_hold() | (1 << 10));
        })?;

        v.modify(dev7.AN_CFG0().AN_CFG0(), |r| {
            r.set_tr_disable(0);
        })?;

        // For now, let's assume we're doing training
        // "Clear training history" (1555)
        v.modify(dev1.TR_CFG0().TR_CFG0(), |r| {
            r.set_sm_hist_clr(0);
        })?;

        // "KR Training config according to UG1061 chapter 3.1" 1563
        v.modify(dev1.TR_MTHD().TR_MTHD(), |r| {
            r.set_mthd_cp(0);
            r.set_mthd_c0(0);
            r.set_mthd_cm(0);
        })?;
        v.modify(dev1.TR_CFG0().TR_CFG0(), |r| {
            r.set_ld_pre_init(1);
            r.set_lp_pre_init(1);
        })?;
        v.modify(dev1.TR_CFG2().TR_CFG2(), |r| {
            r.set_vp_max(0x1f);
            r.set_v2_min(1);
        })?;
        v.modify(dev1.TR_CFG3().TR_CFG3(), |r| {
            r.set_cp_max(0x3f);
            r.set_cp_min(0x35);
        })?;
        v.modify(dev1.TR_CFG4().TR_CFG4(), |r| {
            r.set_c0_max(0x1f);
            r.set_c0_min(0xc);
        })?;
        v.modify(dev1.TR_CFG5().TR_CFG5(), |r| {
            r.set_cm_max(0);
            r.set_cm_min(0x3a);
        })?;
        v.modify(dev1.TR_CFG6().TR_CFG6(), |r| {
            r.set_cp_init(0x38);
            r.set_c0_init(0x14);
        })?;
        v.modify(dev1.TR_CFG7().TR_CFG7(), |r| {
            r.set_cm_init(0x3e);
        })?;
        v.modify(dev1.OBCFG_ADDR().OBCFG_ADDR(), |r| {
            r.set_obcfg_addr(0x12);
        })?;

        // "KR Autoneg" (line 1626)
        // For now, operate under the assumption that we *are* doing aneg

        // "Disable clock gating"
        v.modify(dev7.AN_CFG0().AN_CFG0(), |r| r.set_clkg_disable(0))?;
        // "Clear aneg history"
        v.modify(dev7.AN_CFG0().AN_CFG0(), |r| r.set_an_sm_hist_clr(1))?;
        v.modify(dev7.AN_CFG0().AN_CFG0(), |r| r.set_an_sm_hist_clr(0))?;
        // "Disable / Enable Auto-neg"
        v.modify(dev7.KR_7X0000().KR_7X0000(), |r| r.set_an_enable(0))?;
        v.modify(dev7.KR_7X0000().KR_7X0000(), |r| r.set_an_enable(1))?;

        // "Release the break link timer"
        v.modify(dev1.TR_CFG1().TR_CFG1(), |r| {
            let mut tmr_hold = r.tmr_hold();
            tmr_hold &= !(1 << 10);
            r.set_tmr_hold(tmr_hold);
        })?;
        Ok(())
    }

    /// Checks the 10GBASE-KR autonegotiation state machine
    ///
    /// If it is stuck in `WAIT_RATE_DONE`, restarts autonegotiation and returns
    /// `Ok(true)`, otherwise returns `Ok(false)`.
    pub fn check_10gbase_kr_aneg(
        &self,
        v: &impl Vsc7448Rw,
    ) -> Result<bool, VscError> {
        let sm_state = v.read(XGKR1(self.index()).AN_SM().AN_SM())?;
        // The autonegotiation state machine will occasionally get stuck in
        // WAIT_RATE_DONE.  If that's the case, then we kick it here.
        const WAIT_RATE_DONE: u32 = 13;
        if sm_state.an_sm() == WAIT_RATE_DONE {
            let dev7 = XGKR1(self.index()); // ANEG

            // Clear autonegotiation state machine history, which is oddly
            // load-bearing (??)
            v.modify(dev7.AN_CFG0().AN_CFG0(), |r| r.set_an_sm_hist_clr(1))?;
            v.modify(dev7.AN_CFG0().AN_CFG0(), |r| r.set_an_sm_hist_clr(0))?;

            // Re-trigger autonegotiation
            v.modify(dev7.KR_7X0000().KR_7X0000(), |r| {
                r.set_an_enable(1);
                r.set_an_restart(1);
            })?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
