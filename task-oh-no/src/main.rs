#![no_std]
#![no_main]

use cortex_m_semihosting::hprintln;
use userlib::*;
use zerocopy::AsBytes;

#[cfg(feature = "standalone")]
const OHNO2: Task = Task::anonymous;

#[cfg(not(feature = "standalone"))]
const OHNO2: Task = Task::oh_no2;

#[export_name = "main"]
fn main() -> ! {
    let mut ohno2 =
        TaskId::for_index_and_gen(OHNO2 as usize, Generation::default());

    loop {
        hprintln!("task1: making a call").ok();
        #[derive(AsBytes)]
        #[repr(C)]
        struct AskNicely;

        impl hl::Call for AskNicely {
            const OP: u16 = 1;
            type Response = ();
            type Err = u32;
        }

        let response = hl::send(ohno2, &AskNicely);

        match response {
            Ok(_) => {
                hprintln!("task1: got ok").ok();
            }
            Err(e) => {
                if e >= !0xFF {
                    hprintln!("task1: callee died: {:x}", e).ok();
                    let new_gen = (e & 0xFF) as u8;
                    hprintln!(
                        "task1: starting over with new generation: {}",
                        new_gen
                    )
                    .ok();
                    ohno2 = TaskId::for_index_and_gen(
                        OHNO2 as usize,
                        Generation::from(new_gen),
                    );
                } else {
                    hprintln!("task1: got some other error: {:x}", e).ok();
                }
            }
        }

        hl::sleep_for(10);
    }
}
