//! Dogsitter
//!
//! This task's job is to feed the watchdog.
//!
//! For now, it unconditionally feeds the watchdog. Eventually, we will want
//! to have this task try and determine if the system is broken, and only
//! feed it when things seem fine, but for now, we're not doing that.

#![no_std]
#![no_main]

use userlib::*;
use zerocopy::AsBytes;

#[cfg(feature = "standalone")]
const WWDT: Task = SELF;

#[cfg(not(feature = "standalone"))]
const WWDT: Task = Task::wwdt_driver;

#[export_name = "main"]
fn main() -> ! {
    let wwdt = TaskId::for_index_and_gen(WWDT as usize, Generation::default());

    loop {
        #[derive(AsBytes)]
        #[repr(C)]
        struct FeedWwdt;

        impl hl::Call for FeedWwdt {
            const OP: u16 = 1;
            type Response = ();
            type Err = u32;
        }

        // TODO: we probably only want to feed the watchdog after doing some checks of some kind,
        // but for now, unconditionally feed.
        hl::send(wwdt, &FeedWwdt).expect("could not ask the wwdt to feed");

        // TODO: how long should we sleep here?
        hl::sleep_for(10);
    }
}
