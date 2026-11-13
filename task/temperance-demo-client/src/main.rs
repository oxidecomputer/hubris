// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! TODO: Add docs

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

use postcard_schema_ng::key::Key;
use static_cell::ClaimOnceCell;
use task_temperance_demo_api::{
    AddError, AddRequest, FillRequest, MaxLenByteSlice, all_rpcs,
    demo::{self, Client as _},
    demo_bulk::Client as _,
    demo_upload::Client as _,
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
        #[cfg(target_os = "none")]
        let _ = format_args!($($arg)*);
    };
}

autobuffer!(AllBufs, all_rpcs);

static BUFS: ClaimOnceCell<AllBufs> = ClaimOnceCell::new(AllBufs::new());

const SMALL_LEN: usize = 16;
const LARGE_LEN: usize = 400;

const PERIOD: u64 = 100;
const LOG_EVERY: u32 = 10;

fn checksum(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |acc, &b| acc.rotate_left(5) ^ u32::from(b))
}

static LARGE: ClaimOnceCell<[u8; LARGE_LEN]> =
    ClaimOnceCell::new([0; LARGE_LEN]);

fn fill_pattern(buf: &mut [u8], round: u32) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i as u32).wrapping_add(round) as u8;
    }
}

fn pattern<const N: usize>(round: u32) -> [u8; N] {
    let mut buf = [0; N];
    fill_pattern(&mut buf, round);
    buf
}

fn byte_sum(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |acc, &b| acc.wrapping_add(u32::from(b)))
}

fn upload_checks(
    client: &mut IpcClient<AllBufs>,
    large: &mut [u8; LARGE_LEN],
    round: u32,
) {
    let small: [u8; SMALL_LEN] = pattern(round);
    let rqst = MaxLenByteSlice::try_from(&small).unwrap();
    assert_eq!(client.sum(&rqst).unwrap().body, byte_sum(&small));

    fill_pattern(large, round);
    let rqst = MaxLenByteSlice::try_from(&*large).unwrap();
    assert_eq!(client.sum(&rqst).unwrap().body, byte_sum(&large[..]));
}

fn bulk_checks(
    client: &mut IpcClient<AllBufs>,
    large: &mut [u8; LARGE_LEN],
    round: u32,
) {
    let byte = round as u8;
    let fill = FillRequest {
        byte,
        len: LARGE_LEN as u16,
    };
    let resp = client.fill(&fill).unwrap().body;
    assert_eq!(resp.len(), LARGE_LEN);
    assert!(resp.iter().all(|&b| b == byte));

    let small: [u8; SMALL_LEN] = pattern(round);
    let rqst = MaxLenByteSlice::try_from(&small).unwrap();
    assert_eq!(&*client.echo(&rqst).unwrap().body, &small[..]);

    fill_pattern(large, round);
    let rqst = MaxLenByteSlice::try_from(&*large).unwrap();
    assert_eq!(&*client.echo(&rqst).unwrap().body, &large[..]);
}

fn demo_checks(
    client: &mut IpcClient<AllBufs>,
    round: u32,
    expected_total: &mut u64,
) {
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
    *expected_total += u64::from(round);
    assert_eq!(total, *expected_total);

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
        ClientIoError::Client(ClientError::Server(WireError::SERVER_BAD_BODY))
    );
}

#[cfg_attr(target_os = "none", unsafe(export_name = "main"))]
fn main() -> ! {
    let server = SERVER.get_task_id();
    let mut client = IpcClient::new(server, BUFS.claim());
    let large = LARGE.claim();
    let mut expected_total = 0u64;
    let mut round = 0u32;

    log!("started");
    loop {
        round = round.wrapping_add(1);
        demo_checks(&mut client, round, &mut expected_total);
        upload_checks(&mut client, large, round);
        bulk_checks(&mut client, large, round);
        if round % LOG_EVERY == 1 {
            log!("round {round} ok, total {expected_total}");
        }
        sleep_for(PERIOD);
    }
}
