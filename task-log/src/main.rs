#![no_std]
#![no_main]

use byteorder::BigEndian;
use userlib::*;
use zerocopy::{AsBytes, U16};

#[derive(Debug)]
#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

#[derive(AsBytes)]
#[repr(C)]
struct LogHeader {
    magic: U16<BigEndian>,
    caller_id: U16<BigEndian>,
    message_len: U16<BigEndian>,
}

const BUFFER_LEN: usize = 2048;

#[export_name = "main"]
fn main() -> ! {
    let mut message_buffer = [0u32; 1];

    let stim = unsafe { &mut (*cortex_m::peripheral::ITM::ptr()).stim[0] };

    let mut buffer: [u8; BUFFER_LEN] = [0; BUFFER_LEN];
    let mut write_pos = 0;

    loop {
        hl::recv_without_notification(
            message_buffer.as_bytes_mut(),
            |_op: u16, msg| -> Result<(), ResponseCode> {
                let (_, caller) = msg
                    .fixed_with_leases::<(), ()>(1)
                    .ok_or(ResponseCode::BadArg)?;

                let borrow = caller.borrow(0);
                let info = borrow.info().unwrap();
                let message_len = info.len;

                let mut contents = [0; 256];

                borrow
                    .read_fully_at(0, &mut contents[..message_len])
                    .unwrap();
                let contents = &contents[..message_len];

                let caller_id = caller.task_id().0;

                let header = LogHeader {
                    magic: U16::new(0x01de),
                    caller_id: U16::new(caller_id),
                    message_len: U16::new(message_len as u16),
                };

                let total_size =
                    core::mem::size_of::<LogHeader>() + message_len;
                let header_bytes = header.as_bytes();

                // header
                for (idx, &byte) in header_bytes.iter().enumerate() {
                    buffer[next_index(write_pos, idx)] = byte;
                }
                cortex_m::itm::write_all(stim, header_bytes);

                // message
                for (idx, &byte) in contents.iter().enumerate() {
                    buffer[next_index(write_pos, 6 + idx)] = byte;
                }
                cortex_m::itm::write_all(stim, &contents);

                // update position
                write_pos = next_index(write_pos, total_size);

                caller.reply(());
                Ok(())
            },
        );
    }
}

// modular arithmetic for indexing into our buffer
fn next_index(write_pos: usize, num: usize) -> usize {
    (write_pos + num) % BUFFER_LEN
}
