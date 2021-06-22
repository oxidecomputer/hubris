#![no_std]
#![no_main]

use ringbuf::*;
use userlib::*;

#[cfg(feature = "standalone")]
const SPI: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const SPI: Task = Task::spi_driver;

#[derive(Copy, Clone, PartialEq)]
enum Payload {
    None,
    Calling,
    Returned([u8; 4]),
    Error(u32),
}

ringbuf!(Payload, 16, Payload::None);

#[export_name = "main"]
fn main() -> ! {
    let spi = get_task_id(SPI);
    sys_log!("Waiting to receive SPI data");
    loop {
        let mut recv: [u8; 4] = [0; 4];
        let b: &mut [u8] = &mut recv;

        cfg_if::cfg_if! {
            if #[cfg(target_board = "gemini-bu-rot-1")] {
                let buf : [u8; 4] = [0xCA, 0xFE, 0xFE, 0xED];
            } else {
                let buf : [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
            }
        }

        let op = 3;
        let a: &[u8] = &buf;
        ringbuf_entry!(Payload::Calling);
        let (code, _) =
            sys_send(spi, op, &[], &mut [], &[Lease::from(a), Lease::from(b)]);
        if code != 0 {
            ringbuf_entry!(Payload::Error(code));
        } else {
            ringbuf_entry!(Payload::Returned(recv));
        }
    }
}
