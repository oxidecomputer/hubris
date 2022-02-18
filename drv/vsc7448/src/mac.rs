// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
use crate::{spi::Vsc7448Spi, VscError};
use userlib::hl;
use vsc7448_pac::Vsc7448;

/// Represents an entry in the VSC7448's MAC tables
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MacTableEntry {
    pub mac: [u8; 6],
    src_kill_fwd: bool,
    addr: u16,
    addr_type: u8,
    nxt_lrn_all: bool,
    cpu_copy: bool,
    vlan_ignore: bool,
    age_flag: u8,
    age_interval: u8,
    mirror: bool,
    locked: bool,
    valid: bool,
}

pub fn next_mac(v: &Vsc7448Spi) -> Result<Option<MacTableEntry>, VscError> {
    // Trigger a FIND_SMALLEST action then wait for it to finish
    let ctrl = Vsc7448::LRN().COMMON().COMMON_ACCESS_CTRL();
    v.write_with(ctrl, |r| {
        r.set_cpu_access_cmd(0x6); // FIND_SMALLEST
        r.set_mac_table_access_shot(0x1); // run
    })?;
    while v.read(ctrl)?.mac_table_access_shot() == 1 {
        hl::sleep_for(1);
    }

    let msb = v
        .read(Vsc7448::LRN().COMMON().MAC_ACCESS_CFG_0())?
        .mac_entry_mac_msb();
    let lsb = v
        .read(Vsc7448::LRN().COMMON().MAC_ACCESS_CFG_1())?
        .mac_entry_mac_lsb();
    let cfg = v.read(Vsc7448::LRN().COMMON().MAC_ACCESS_CFG_2())?;
    if msb == 0 && lsb == 0 {
        Ok(None)
    } else {
        let mut out = MacTableEntry::default();
        out.mac[0..2].copy_from_slice(&msb.to_be_bytes()[2..]);
        out.mac[2..6].copy_from_slice(&lsb.to_be_bytes());

        out.src_kill_fwd = cfg.mac_entry_src_kill_fwd() != 0;
        out.addr = cfg.mac_entry_addr() as u16;
        out.addr_type = cfg.mac_entry_addr_type() as u8;
        out.nxt_lrn_all = cfg.mac_entry_nxt_lrn_all() != 0;
        out.cpu_copy = cfg.mac_entry_cpu_copy() != 0;
        out.vlan_ignore = cfg.mac_entry_vlan_ignore() != 0;
        out.age_flag = cfg.mac_entry_age_flag() as u8;
        out.age_interval = cfg.mac_entry_age_interval() as u8;
        out.mirror = cfg.mac_entry_mirror() != 0;
        out.locked = cfg.mac_entry_locked() != 0;
        out.valid = cfg.mac_entry_vld() != 0;

        Ok(Some(out))
    }
}
