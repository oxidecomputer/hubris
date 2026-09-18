// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Drives a client and a server over a pair of pipes, the way a task process
//! and a fixture talk over stdio.

use std::io::BufReader;

use hostcall::{
    Client, Fault, IoError, Lease, Outcome, RecvMessage, RecvRequest,
    SendRequest, SendResponse, Server, all, runtime, syscalls,
};
use nprpc::{Request, ServerIoError};

/// A fixture that echoes sends, answers receives with a canned message, and
/// knows one task slot.
struct Echo {
    log: Vec<String>,
}

impl syscalls::Server for Echo {
    fn send(
        &mut self,
        rqst: Request<'_, SendRequest>,
    ) -> Outcome<SendResponse> {
        self.log.push(format!(
            "send to {} op {}",
            rqst.body.target, rqst.body.operation
        ));
        Ok(SendResponse {
            code: rqst.body.message.len() as u32,
            reply: rqst.body.message.iter().rev().copied().collect(),
            lease_writebacks: rqst
                .body
                .leases
                .iter()
                .map(|l| {
                    (l.attributes & 2 != 0).then(|| vec![0xAA; l.len as usize])
                })
                .collect(),
        })
    }

    fn recv(
        &mut self,
        rqst: Request<'_, RecvRequest>,
    ) -> Outcome<hostcall::RecvResponse> {
        Ok(Ok(RecvMessage {
            sender: 7,
            operation: 1,
            message_len: 3,
            message: vec![1, 2, 3],
            response_capacity: rqst.body.capacity,
            lease_count: 0,
        }))
    }

    fn reply(&mut self, _: Request<'_, hostcall::ReplyRequest>) -> Outcome<()> {
        Ok(())
    }

    fn set_timer(
        &mut self,
        _: Request<'_, hostcall::SetTimerRequest>,
    ) -> Outcome<()> {
        Ok(())
    }

    fn borrow_read(
        &mut self,
        _: Request<'_, hostcall::BorrowReadRequest>,
    ) -> Outcome<hostcall::BorrowReadResponse> {
        Err(Fault {
            description: "no leases here".into(),
        })
    }

    fn borrow_write(
        &mut self,
        _: Request<'_, hostcall::BorrowWriteRequest>,
    ) -> Outcome<hostcall::BorrowWriteResponse> {
        Err(Fault {
            description: "no leases here".into(),
        })
    }

    fn borrow_info(
        &mut self,
        _: Request<'_, hostcall::BorrowInfoRequest>,
    ) -> Outcome<Option<hostcall::BorrowInfo>> {
        Ok(None)
    }

    fn irq_control(
        &mut self,
        _: Request<'_, hostcall::IrqControlRequest>,
    ) -> Outcome<()> {
        Ok(())
    }

    fn panic(&mut self, rqst: Request<'_, hostcall::PanicRequest>) {
        self.log.push(format!(
            "panic: {}",
            String::from_utf8_lossy(&rqst.body.message)
        ));
    }

    fn get_timer(
        &mut self,
        _: Request<'_, ()>,
    ) -> Outcome<hostcall::TimerState> {
        Ok(hostcall::TimerState {
            now: 42,
            deadline: None,
            on_deadline: 0,
        })
    }

    fn refresh_task_id(&mut self, rqst: Request<'_, u16>) -> Outcome<u16> {
        Ok(*rqst.body)
    }

    fn post(&mut self, _: Request<'_, hostcall::PostRequest>) -> Outcome<u32> {
        Ok(0)
    }

    fn reply_fault(
        &mut self,
        _: Request<'_, hostcall::ReplyFaultRequest>,
    ) -> Outcome<()> {
        Ok(())
    }

    fn irq_status(&mut self, _: Request<'_, u32>) -> Outcome<u32> {
        Ok(0)
    }
}

impl runtime::Server for Echo {
    fn task_slot(&mut self, rqst: Request<'_, String>) -> Outcome<u16> {
        match rqst.body.as_str() {
            "peer" => Ok(3),
            other => Err(Fault {
                description: format!("unknown slot {other}"),
            }),
        }
    }
}

#[test]
fn client_and_server_over_pipes() {
    use runtime::Client as _;
    use syscalls::Client as _;

    let (to_server_rx, to_server_tx) = std::io::pipe().unwrap();
    let (to_client_rx, to_client_tx) = std::io::pipe().unwrap();

    let server = std::thread::spawn(move || {
        let mut backend =
            Server::new(BufReader::new(to_server_rx), to_client_tx);
        let mut echo = Echo { log: Vec::new() };
        loop {
            match <Echo as all::Server>::serve_one(&mut echo, &mut backend) {
                Ok(()) => {}
                Err(ServerIoError::Io(IoError::Eof)) => break,
                Err(e) => panic!("server error: {e:?}"),
            }
        }
        echo.log
    });

    let mut client = Client::new(BufReader::new(to_client_rx), to_server_tx);

    let resp = client
        .send(&SendRequest {
            target: 3,
            operation: 9,
            message: b"hello".to_vec(),
            reply_capacity: 16,
            leases: vec![
                Lease {
                    attributes: 1,
                    len: 4,
                    contents: vec![1, 2, 3, 4],
                },
                Lease {
                    attributes: 2,
                    len: 2,
                    contents: vec![],
                },
            ],
        })
        .unwrap();
    assert_eq!(
        resp.body,
        Ok(SendResponse {
            code: 5,
            reply: b"olleh".to_vec(),
            lease_writebacks: vec![None, Some(vec![0xAA, 0xAA])],
        })
    );

    let resp = client
        .recv(&RecvRequest {
            capacity: 2,
            notification_mask: 0,
            specific_sender: None,
        })
        .unwrap();
    let msg = resp.body.unwrap().unwrap();
    assert_eq!(
        (
            msg.sender,
            msg.operation,
            msg.message_len,
            msg.response_capacity
        ),
        (7, 1, 3, 2)
    );

    assert_eq!(client.task_slot(&"peer".to_string()).unwrap().body, Ok(3));
    let fault = client
        .task_slot(&"nope".to_string())
        .unwrap()
        .body
        .unwrap_err();
    assert_eq!(fault.description, "unknown slot nope");

    let fault = client
        .borrow_info(&hostcall::BorrowInfoRequest {
            lender: 1,
            index: 0,
        })
        .unwrap();
    assert_eq!(fault.body, Ok(None));

    assert_eq!(resp.hedr.seqno, 1, "sequence numbers count requests");

    client
        .panic(&hostcall::PanicRequest {
            message: b"boom".to_vec(),
        })
        .unwrap();
    drop(client);

    let log = server.join().unwrap();
    assert_eq!(log, vec!["send to 3 op 9", "panic: boom"]);
}
