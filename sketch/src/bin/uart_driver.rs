//! This is a sketch of how a UART driver might be constructed.
//!
//! The UART hardware here is vaguely modeled on the 16550.
//!
//! # Protocol
//!
//! This driver implements the following IPC protocol.
//!
//! The first byte of *any* response is a success code: 0 for operation
//! performed, 1 for operation not recognized, 2 for resource exhaustion.
//!
//! ## `read`
//!
//! `message[0] == 0`
//!
//! Collects a single character from the UART into the response buffer. Blocks
//! as needed.
//!
//! `response[0] == 1`
//! `response[1] == c`
//!
//! ## `write`
//!
//! `message[0] == 1`
//! `message[1] == c`
//!
//! Sends a single character `c` to the UART. Blocks as needed.
//!
//! `response[0] == 1`
//!
//! # Notification binding
//!
//! Internally, we map hardware interrupts to notification bits as follows:
//!
//! - 0: `TxE`: transmit holding register empty
//! - 1: `RxNE`: receive holding register not empty

#![no_std]
#![no_main]

// you can put a breakpoint on `rust_begin_unwind` to catch panics
extern crate panic_halt;

// logs messages over ITM; requires ITM support
//extern crate panic_itm;

use arrayvec::ArrayVec;
use byteorder::{ByteOrder, LittleEndian};
use sketch::*;

/// Size of wait queues to allocate. Would presumably be compile-time
/// configurable in a real system.
const MAX_CLIENTS: usize = 4;

/// Bit mask for TxE notification.
const TXE_NOTIFICATION: u32 = 1 << 0;
/// Bit mask for RxNE notification.
const RXNE_NOTIFICATION: u32 = 1 << 1;

/// Operation code for read.
const READ_OP: u8 = 0;
/// Operation code for write.
const WRITE_OP: u8 = 1;

/// Response code for "success"
const SUCCESS: u32 = 0;
/// Response code for "unknown operation code"
const UNKNOWN_OP: u32 = 1;
/// Response code for "resources exhausted"
const EXHAUSTED: u32 = 2;

#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    safe_main()
}

fn safe_main() -> ! {
    // Note: our interrupts are initially disabled by the kernel.

    // Initialize the peripheral.
    hw_setup();

    // Set up interrupt masks. Initially, we have no callers waiting to either
    // transmit or receive, so we are not interested in any notifications. Mask
    // our notifications and leave interrupts off.
    set_notification_mask(0);

    // Main loop.
    let mut buffer = [0; 4];
    let mut blocked_in_rx = ArrayVec::<[TaskName; MAX_CLIENTS]>::new();
    let mut blocked_in_tx = ArrayVec::<[(TaskName, u8); MAX_CLIENTS]>::new();

    loop {
        // Receive any incoming event, either from clients or from
        // notifications.
        let message_info = receive(&mut buffer);

        if message_info.sender == TaskName(0) {
            // Notification message from the kernel. See which bits were
            // pending.
            let bits = LittleEndian::read_u32(&buffer);

            if bits & TXE_NOTIFICATION != 0 {
                // Transmit holding register has become empty. We can unblock a
                // client.
                if let Some((sender, c)) = blocked_in_tx.pop() {
                    write_thr(c);
                    reply(sender, SUCCESS, &[]);
                } else {
                    // We left TxE enabled without any clients queued? That's a
                    // bug.
                    panic!()
                }
                if blocked_in_tx.is_empty() {
                    // Everyone's handled, mask the interrupt
                    mask_notifications(TXE_NOTIFICATION);
                } else {
                    // We'd be interested in further interrupts like this.
                    enable_interrupts(TXE_NOTIFICATION);
                }
            }

            if bits & RXNE_NOTIFICATION != 0 {
                // Receive holding register has become non-empty.
                if let Some(sender) = blocked_in_rx.pop() {
                    reply(sender, SUCCESS, &[read_rbr()]);
                } else {
                    // We left RxNE enabled without any clients queued? That's a
                    // bug.
                    panic!()
                }
                if blocked_in_rx.is_empty() {
                    // Everyone's handled, mask the interrupt
                    mask_notifications(RXNE_NOTIFICATION);
                } else {
                    // We'd be interested in further interrupts like this.
                    enable_interrupts(RXNE_NOTIFICATION);
                }
            }
        } else {
            // Interprocess message from a client
            match buffer[0] {
                READ_OP => {
                    // Read

                    // If the receive holding register is not empty, respond
                    // promptly.
                    if rbr_full() {
                        reply(message_info.sender, SUCCESS, &[read_rbr()]);
                    } else {
                        // Otherwise we need to block the caller.
                        if let Err(_) =
                            blocked_in_rx.try_push(message_info.sender)
                        {
                            // Send back resource exhaustion code.
                            reply(message_info.sender, EXHAUSTED, &[]);
                        } else {
                            // Enable the notification and IRQ. They may already
                            // be enabled; these operations are idempotent and
                            // cheaper than checking.
                            unmask_notifications(RXNE_NOTIFICATION);
                            enable_interrupts(RXNE_NOTIFICATION);
                        }
                    }
                }
                WRITE_OP => {
                    // Write
                    let c = buffer[1];

                    // If the transmit holding register is empty, respond
                    // promptly.
                    if thr_empty() {
                        write_thr(c);
                        reply(message_info.sender, SUCCESS, &[]);
                    } else {
                        // Otherwise we need to block the caller.
                        if let Err(_) =
                            blocked_in_tx.try_push((message_info.sender, c))
                        {
                            // Send back resource exhaustion code.
                            reply(message_info.sender, EXHAUSTED, &[]);
                        } else {
                            // Enable the notification and IRQ. They may already
                            // be enabled; these operations are idempotent and
                            // cheaper than checking.
                            unmask_notifications(TXE_NOTIFICATION);
                            enable_interrupts(TXE_NOTIFICATION);
                        }
                    }
                }
                _ => {
                    // Unknown operation
                    reply(message_info.sender, UNKNOWN_OP, &[]);
                }
            }
        }
    }
}

/////////// stub peripheral interface starts here

fn hw_setup() {
}

// fake hardware registers
static mut RBR: u8 = 0;
static mut RBR_FULL: bool = false;
static mut THR: u8 = 0;
static mut THR_EMPTY: bool = true;

fn rbr_full() -> bool {
    unsafe {
        core::ptr::read_volatile(&RBR_FULL)
    }
}

fn read_rbr() -> u8 {
    unsafe {
        core::ptr::read_volatile(&RBR)
    }
}

fn thr_empty() -> bool {
    unsafe {
        core::ptr::read_volatile(&THR_EMPTY)
    }
}

fn write_thr(c: u8) {
    unsafe {
        core::ptr::write_volatile(&mut THR, c)
    }
}
