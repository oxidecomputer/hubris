// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Server for managing the Sidecar sequencing process.

#![no_std]
#![no_main]

use drv_local_vpd::{BarcodeParseError, LocalVpdError, VpdIdentityError};
use drv_psc_seq_api::PowerState;
use ringbuf::*;
use task_jefe_api::Jefe;
use task_packrat_api::{CacheSetError, MacAddressBlock, Packrat, VpdIdentity};
use userlib::*;

task_slot!(I2C, i2c_driver);
task_slot!(JEFE, jefe);
task_slot!(PACKRAT, packrat);

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
enum Trace {
    None,
    LocalVpdError(LocalVpdError),
    BarcodeParseError(BarcodeParseError),
    MacsAlreadySet(MacAddressBlock),
    IdentityAlreadySet(VpdIdentity),
}
ringbuf!(Trace, 32, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    let jefe = Jefe::from(JEFE.get_task_id());

    // Populate packrat with our mac address and identity.
    read_vpd();

    jefe.set_state(PowerState::A2 as u32);

    // We have nothing else to do, so sleep forever via waiting for a message
    // from the kernel that won't arrive.
    loop {
        _ = sys_recv_closed(&mut [], 0, TaskId::KERNEL);
    }
}

fn read_vpd() {
    // How many times are we willing to try reading VPD if it fails, and how
    // long do we wait between retries?
    const MAX_ATTEMPTS: usize = 5;
    const SLEEP_BETWEEN_RETRIES_MS: u64 = 500;

    let mut read_macs = false;
    let mut read_identity = false;
    let i2c = I2C.get_task_id();
    let packrat = Packrat::from(PACKRAT.get_task_id());

    for _ in 0..MAX_ATTEMPTS {
        if !read_macs {
            match drv_local_vpd::read_config(i2c, *b"MAC0") {
                Ok(macs) => {
                    match packrat.set_mac_address_block(macs) {
                        Ok(()) => (),
                        Err(CacheSetError::ValueAlreadySet) => {
                            ringbuf_entry!(Trace::MacsAlreadySet(macs));
                        }
                    }
                    read_macs = true;
                }
                Err(err) => {
                    ringbuf_entry!(Trace::LocalVpdError(err));
                }
            }
        }

        if !read_identity {
            match drv_local_vpd::read_oxide_barcode(i2c) {
                Ok(identity) => {
                    match packrat.set_identity(identity) {
                        Ok(()) => (),
                        Err(CacheSetError::ValueAlreadySet) => {
                            ringbuf_entry!(Trace::IdentityAlreadySet(identity));
                        }
                    }
                    read_identity = true;
                }
                Err(VpdIdentityError::LocalVpdError(err)) => {
                    ringbuf_entry!(Trace::LocalVpdError(err));
                }
                Err(VpdIdentityError::ParseError(err)) => {
                    ringbuf_entry!(Trace::BarcodeParseError(err));
                }
            }
        }

        if read_macs && read_identity {
            break;
        }

        hl::sleep_for(SLEEP_BETWEEN_RETRIES_MS);
    }
}
