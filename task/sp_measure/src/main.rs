// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![no_main]

use attest_api::{Attest, AttestError, HashAlgorithm};
use drv_sp_ctrl_api::*;
use ringbuf::*;
use sha3::{Digest, Sha3_256};
use userlib::*;
use zerocopy::AsBytes;

const READ_SIZE: usize = 256;

const TRANSACTION_SIZE: u32 = 1024;

task_slot!(ATTEST, attest);
task_slot!(SP_CTRL, swd);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Start(u64),
    End(u64),
    RecordFail(AttestError),
    None,
}

ringbuf!(Trace, 16, Trace::None);

#[export_name = "main"]
fn main() -> ! {
    loop {
        let mut sha = Sha3_256::new();
        let sp_ctrl = SpCtrl::from(SP_CTRL.get_task_id());

        if sp_ctrl.setup().is_err() {
            panic!();
        }

        let mut data: [u8; READ_SIZE] = [0; READ_SIZE];

        let start = sys_get_timer().now;
        ringbuf_entry!(Trace::Start(start));
        for addr in (FLASH_START..FLASH_END).step_by(READ_SIZE) {
            if addr % TRANSACTION_SIZE == 0
                && sp_ctrl
                    .read_transaction_start(addr, addr + TRANSACTION_SIZE)
                    .is_err()
            {
                panic!();
            }

            data.fill(0);
            if sp_ctrl.read_transaction(&mut data).is_err() {
                panic!();
            }

            sha.update(&data);
        }

        let sha_out = sha.finalize();

        let end = sys_get_timer().now;
        ringbuf_entry!(Trace::End(end));

        let attest = Attest::from(ATTEST.get_task_id());
        if let Err(e) =
            attest.record(HashAlgorithm::Sha3_256, sha_out.as_bytes())
        {
            ringbuf_entry!(Trace::RecordFail(e));
            panic!();
        };

        // Wait for a notification that will never come, politer than
        // busy looping forever
        if sys_recv_closed(&mut [], 1, TaskId::KERNEL).is_err() {
            panic!();
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/expected.rs"));
