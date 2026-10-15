// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Exchanges messages over a pair of pipes, the way a host-native `kern` and
//! `userlib` do over stdio.

use std::io::{BufReader, Write};

use hostcall::{
    IoError, Lease, Return, Syscall, read_message, task_slots_value,
    write_message,
};

#[test]
fn syscall_and_return_over_pipes() {
    let (to_helper_rx, mut to_helper_tx) = std::io::pipe().unwrap();
    let (to_task_rx, to_task_tx) = std::io::pipe().unwrap();

    // A helper that answers each syscall with its input reversed, and
    // fills every writable lease.
    let helper = std::thread::spawn(move || {
        let mut from_task = BufReader::new(to_helper_rx);
        let mut to_task = to_task_tx;
        let mut seen = Vec::new();
        while let Some(call) = read_message::<Syscall>(&mut from_task).unwrap()
        {
            let ret = Return {
                rets: [call.args[0], call.input.len() as u32, 0, 0, 0, 0],
                output: call.input.iter().rev().copied().collect(),
                lease_writebacks: call
                    .leases
                    .iter()
                    .map(|l| {
                        (l.attributes.0 & 2 != 0)
                            .then(|| vec![0xAA; l.len as usize])
                    })
                    .collect(),
            };
            seen.push(call);
            write_message(&mut to_task, &ret).unwrap();
        }
        seen
    });

    let mut from_helper = BufReader::new(to_task_rx);
    let call = Syscall {
        nr: abi::Sysnum::Send,
        args: [3 << 16 | 9, 0, 5, 0, 16, 0, 2],
        // Ensure 0-bytes are escaped correctly
        input: b"he\0lo".to_vec(),
        output_len: 16,
        leases: vec![
            Lease {
                attributes: abi::LeaseAttributes(1),
                len: 4,
                contents: vec![1, 2, 3, 4],
            },
            Lease {
                attributes: abi::LeaseAttributes(2),
                len: 2,
                contents: vec![],
            },
        ],
    };
    write_message(&mut to_helper_tx, &call).unwrap();
    let ret: Return = read_message(&mut from_helper).unwrap().unwrap();
    assert_eq!(
        ret,
        Return {
            rets: [3 << 16 | 9, 5, 0, 0, 0, 0],
            output: b"ol\0eh".to_vec(),
            lease_writebacks: vec![None, Some(vec![0xAA, 0xAA])],
        }
    );

    let empty = Syscall {
        nr: abi::Sysnum::GetTimer,
        args: [0u32; _],
        input: vec![],
        output_len: 0,
        leases: vec![],
    };
    write_message(&mut to_helper_tx, &empty).unwrap();
    let ret: Return = read_message(&mut from_helper).unwrap().unwrap();
    assert_eq!(ret, Return::default());

    // Closing the task's end is a clean end of stream for the helper.
    drop(to_helper_tx);
    assert_eq!(helper.join().unwrap(), vec![call, empty]);
    assert!(read_message::<Return>(&mut from_helper).unwrap().is_none());
}

#[test]
fn bad_messages_are_errors() {
    // Not COBS: a zero-length frame.
    let mut stream = BufReader::new(&[0u8][..]);
    assert!(matches!(
        read_message::<Syscall>(&mut stream),
        Err(IoError::Decode)
    ));

    // COBS, but not a `Syscall`.
    let mut bytes = Vec::new();
    write_message(&mut bytes, &7u8).unwrap();
    assert!(matches!(
        read_message::<Syscall>(&mut BufReader::new(&bytes[..])),
        Err(IoError::Decode)
    ));

    // A stream that ends inside a message.
    let empty = Syscall {
        nr: abi::Sysnum::GetTimer,
        args: [0u32; _],
        input: vec![],
        output_len: 0,
        leases: vec![],
    };
    bytes.clear();
    write_message(&mut bytes, &empty).unwrap();
    bytes.pop();
    bytes.flush().unwrap();
    assert!(matches!(
        read_message::<Syscall>(&mut BufReader::new(&bytes[..])),
        Err(IoError::Eof)
    ));
}

#[test]
fn task_slots_are_listed_by_name() {
    assert_eq!(
        task_slots_value([("peer", 2), ("user_leds", 3)]),
        "peer=2,user_leds=3"
    );
}
