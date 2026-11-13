// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use postcard_schema_ng::key::Key;
use task_temperance_demo_api::{
    AddError, AddRequest, MaxLenByteSlice,
    demo::{self, Client},
};
use temperance::{
    ClientError, ClientIoError, autobuffer, interface::Method,
    io::client::Backend, wire::WireError,
};
use temperance_ipc::IpcClient;
use userlib::{hl::sleep_for, task_slot};

task_slot!(SERVER, server);

// TODO: Add comment
macro_rules! log {
    ($($arg:tt)*) => {
        #[cfg(not(target_os = "none"))]
        eprintln!("demo-client: {}", format_args!($($arg)*));
    };
}

autobuffer!(DemoBufs, demo);

const PERIOD: u64 = 100;
const LOG_EVERY: u32 = 10;

fn checksum(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |acc, &b| acc.rotate_left(5) ^ u32::from(b))
}

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    let mut client = IpcClient::new(SERVER.get_task_id(), DemoBufs::new());
    let mut expected_total = 0u64;
    let mut round = 0u32;

    log!("started");
    loop {
        round = round.wrapping_add(1);

        let sum = client.add(&AddRequest { a: 2, b: round }).unwrap().body;
        assert_eq!(sum, Ok(2 + round));

        let over = client
            .add(&AddRequest {
                a: u32::MAX,
                b: round,
            })
            .unwrap()
            .body;
        assert_eq!(over, Err(AddError::Overflow));

        let total = client.bump(&round).unwrap().body;
        expected_total += u64::from(round);
        assert_eq!(total, expected_total);

        let data = round.to_le_bytes();
        let rqst = MaxLenByteSlice::try_from(&data).unwrap();
        let ck = client.checksum(&rqst).unwrap().body;
        assert_eq!(ck, checksum(&data));

        let bogus = Key::from_bytes([0xA5; 8]);
        let err = client.send_then_receive_raw(bogus, &[]).unwrap_err();
        assert_eq!(
            err,
            ClientIoError::Client(ClientError::Server(
                WireError::SERVER_UNKNOWN_METHOD
            ))
        );

        let add_key = <demo::methods::add as Method>::KEY;
        let err = client.send_then_receive_raw(add_key, &[]).unwrap_err();
        assert_eq!(
            err,
            ClientIoError::Client(ClientError::Server(
                WireError::SERVER_BAD_BODY
            ))
        );

        if round % LOG_EVERY == 1 {
            log!("round {round} ok, total {total}");
        }
        sleep_for(PERIOD);
    }
}
