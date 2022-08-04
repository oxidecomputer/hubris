// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// The following code is based on port_setup in the MESA SDK, but extracted
// and trimmed down to the bare necessacities (e.g. assuming the chip is
// configured from reset)

use crate::{
    dev::{Dev10g, DevGeneric},
    Vsc7448Rw, VscError,
};
use userlib::hl;
use vsc7448_pac::*;

/// Flushes a particular 1G port.  This is equivalent to `jr2_port_flush`
/// in the MESA toolkit.
pub fn port1g_flush(
    dev: &DevGeneric,
    v: &impl Vsc7448Rw,
) -> Result<(), VscError> {
    let port = dev.port();

    // 1: Reset the PCS Rx clock domain
    let dev1g = dev.regs();
    v.modify(dev1g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
        r.set_pcs_rx_rst(1)
    })?;

    // 2: Disable MAC frame reception
    v.modify(dev1g.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| r.set_rx_ena(0))?;

    port_flush_inner(port, v)?;

    // 10: Reset the MAC clock domain
    //
    // Note that the SDK sets PCS Rx/Tx reset to 0 here, meaning they're not
    // actually reset.  However, the manual suggests resetting them, and it
    // doesn't seem to hurt.
    v.modify(dev1g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
        r.set_pcs_rx_rst(1);
        r.set_pcs_tx_rst(1);
        r.set_mac_rx_rst(1);
        r.set_mac_tx_rst(1);
        r.set_speed_sel(3);
    })?;

    // 11: Clear flushing
    v.modify(HSCH().HSCH_MISC().FLUSH_CTRL(), |r| {
        r.set_flush_ena(0);
    })?;
    Ok(())
}

/// Flushes a particular 10G port.  This is equivalent to `jr2_port_flush`
/// in the MESA toolkit.  Unfortunately, it's mostly a copy-pasta from
/// [port_1g_flush], because the registers have similar fields but are
/// different types in our PAC crate.
///
/// `dev` is the 10G device (0-4)
pub fn port10g_flush(dev: &Dev10g, v: &impl Vsc7448Rw) -> Result<(), VscError> {
    let port = dev.port();

    // 1: Reset the PCS Rx clock domain
    let dev10g = dev.regs();
    v.modify(dev10g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
        r.set_pcs_rx_rst(1)
    })?;

    // 2: Disable MAC frame reception
    v.modify(dev10g.MAC_CFG_STATUS().MAC_ENA_CFG(), |r| r.set_rx_ena(0))?;

    port_flush_inner(port, v)?;

    // 10: Reset the MAC clock domain
    v.modify(dev10g.DEV_CFG_STATUS().DEV_RST_CTRL(), |r| {
        r.set_pcs_tx_rst(1);
        r.set_mac_rx_rst(1);
        r.set_mac_tx_rst(1);
        r.set_speed_sel(6);
    })?;

    // 11: Clear flushing
    v.modify(HSCH().HSCH_MISC().FLUSH_CTRL(), |r| {
        r.set_flush_port(port.into());
        r.set_flush_ena(0);
    })?;

    // Bonus for 10G ports: disable XAUI, RXAUI, SFI PCS
    v.modify(dev10g.PCS_XAUI_CONFIGURATION().PCS_XAUI_CFG(), |r| {
        r.set_pcs_ena(0);
    })?;
    v.modify(dev10g.PCS2X6G_CONFIGURATION().PCS2X6G_CFG(), |r| {
        r.set_pcs_ena(0);
    })?;
    v.modify(PCS10G_BR(dev.index()).PCS_10GBR_CFG().PCS_CFG(), |r| {
        r.set_pcs_ena(0);
    })?;

    Ok(())
}

/// Shared logic between 1G and 10G port flushing
fn port_flush_inner(port: u8, v: &impl Vsc7448Rw) -> Result<(), VscError> {
    // 3: Disable traffic being sent to or from switch port
    v.modify(QFWD().SYSTEM().SWITCH_PORT_MODE(port), |r| {
        r.set_port_ena(0)
    })?;

    // 4: Disable dequeuing from the egress queues
    v.modify(HSCH().HSCH_MISC().PORT_MODE(port), |r| r.set_dequeue_dis(1))?;

    // 5: Disable Flowcontrol
    v.modify(QSYS().PAUSE_CFG().PAUSE_CFG(port), |r| r.set_pause_ena(0))?;

    // 5.1: Disable PFC
    v.modify(QRES().RES_QOS_ADV().PFC_CFG(port), |r| r.set_tx_pfc_ena(0))?;

    // 6: Wait a worst case time 8ms (jumbo/10Mbit)
    hl::sleep_for(8);

    // 7: Flush the queues accociated with the port
    v.modify(HSCH().HSCH_MISC().FLUSH_CTRL(), |r| {
        r.set_flush_port(port.into());
        r.set_flush_dst(1);
        r.set_flush_src(1);
        r.set_flush_ena(1);
    })?;

    // 8: Enable dequeuing from the egress queues
    v.modify(HSCH().HSCH_MISC().PORT_MODE(port), |r| r.set_dequeue_dis(0))?;

    // 9: Wait until flushing is complete
    port_flush_wait(port, v)?;

    Ok(())
}

/// Waits for a port flush to finish.  This is based on
/// `jr2_port_flush_poll` in the MESA SDK
fn port_flush_wait(port: u8, v: &impl Vsc7448Rw) -> Result<(), VscError> {
    // This timeout count is based on the SDK, which checks 2000x with a
    // 1 ms pause between attempts.
    for _ in 0..2000 {
        let mut empty = true;
        // DST-MEM and SRC-MEM
        for base in [0, 2048] {
            for prio in 0..8 {
                let value = v.read(
                    QRES()
                        .RES_CTRL(base + 8 * u16::from(port) + prio)
                        .RES_STAT(),
                )?;
                empty &= value.maxuse() == 0;
                // Keep looping, because these registers are clear-on-read,
                // so it's more efficient to read them all, even if we know
                // that the port isn't currently empty.
            }
        }
        if empty {
            return Ok(());
        }
        hl::sleep_for(1);
    }
    Err(VscError::PortFlushTimeout { port })
}
