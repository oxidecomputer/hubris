// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use crate::{Phy, PhyRw, Trace};
use ringbuf::ringbuf_entry_root as ringbuf_entry;
use vsc7448_pac::phy;
use vsc_err::VscError;

/// Based on `vtss_atom_patch_suspend` in the SDK
pub fn atom_patch_suspend<'a, 'b, P: PhyRw>(
    phy: &'b mut Phy<'a, P>,
) -> Result<(), VscError> {
    // We don't have VeriPHY running, so skip the first conditional
    let v = phy.read(phy::GPIO::MICRO_PAGE())?;
    if (v.0 & 0x4000) == 0 {
        phy.cmd(0x800F)?;
        ringbuf_entry!(Trace::AtomPatchSuspend(true));
    } else {
        ringbuf_entry!(Trace::AtomPatchSuspend(false));
    }
    Ok(())
}

/// Based on `vtss_atom_patch_suspend` in the SDK
pub fn atom_patch_resume<'a, 'b, P: PhyRw>(
    phy: &'b mut Phy<'a, P>,
) -> Result<(), VscError> {
    // We don't have VeriPHY running, so skip the first conditional
    let v = phy.read(phy::GPIO::MICRO_PAGE())?;
    if (v.0 & 0x4000) != 0 {
        ringbuf_entry!(Trace::AtomPatchResume(true));
        phy.cmd(0x8009)?;
    } else {
        ringbuf_entry!(Trace::AtomPatchResume(false));
    }
    Ok(())
}
