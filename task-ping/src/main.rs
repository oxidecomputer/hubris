#![no_std]
#![no_main]

use userlib::*;

#[cfg(feature = "standalone")]
const PEER: Task = SELF;

#[cfg(not(feature = "standalone"))]
const PEER: Task = Task::pong;

#[cfg(all(feature = "standalone", feature = "uart"))]
const UART: Task = SELF;

#[cfg(all(not(feature = "standalone"), feature = "uart"))]
const UART: Task = Task::usart_driver;

#[inline(never)]
fn stackblow() {
    let c = [0xdeu8; 8192];
    uart_send(&c[0..1024]);
}

#[export_name = "main"]
fn main() -> ! {
    let user_leds = get_user_leds();

    let peer = TaskId::for_index_and_gen(PEER as usize, Generation::default());
    const PING_OP: u16 = 1;
    let mut response = [0; 16];
    loop {
        uart_send(b"Ping!\r\n");
        // Signal that we're entering send:
        user_leds.led_on(0).unwrap();

        let (code, _len) =
            sys_send(peer, PING_OP, b"hello", &mut response, &[]);

        if code % 2000 == 0 {
            // mwa ha ha ha
            unsafe {
                (0 as *const u8).read_volatile();
            }
        }

        if code % 1000 == 0 {
            // ka-boom
            stackblow();
        }
    }
}

fn get_user_leds() -> drv_user_leds_api::UserLeds {
    #[cfg(not(feature = "standalone"))]
    const USER_LEDS: Task = Task::user_leds;

    #[cfg(feature = "standalone")]
    const USER_LEDS: Task = SELF;

    drv_user_leds_api::UserLeds::from(TaskId::for_index_and_gen(
        USER_LEDS as usize,
        Generation::default(),
    ))
}

#[cfg(feature = "uart")]
fn uart_send(text: &[u8]) {
    let peer = TaskId::for_index_and_gen(UART as usize, Generation::default());

    const OP_WRITE: u16 = 1;
    let (code, _) =
        sys_send(peer, OP_WRITE, &[], &mut [], &[Lease::from(text)]);
    assert_eq!(0, code);
}

#[cfg(not(feature = "uart"))]
fn uart_send(_: &[u8]) {}
