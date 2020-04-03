//! This is a sketch of how a UART driver might be constructed.
//!
//! The UART hardware here is vaguely modeled on the 16550. The driver assumes
//! that it is being operated by a *single* client task at any given time; this
//! is lightly checked. Why a single client task? Well, have you ever tried to
//! share a serial port between threads without further synchronization? It
//! doesn't end well.
//!
//! # Protocol
//!
//! This driver implements the following IPC protocol.
//!
//! The response code of *any* response is as follows: 0 for operation
//! performed, 1 for operation not recognized, 2 for resource exhaustion.
//!
//! ## `read` (0)
//!
//! Collects a single character from the UART into the response buffer. Blocks
//! as needed.
//!
//! `response[0] == c`
//!
//! ## `write` (1)
//!
//! `message[0] == c`
//!
//! Sends a single character `c` to the UART. Blocks as needed.
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

use byteorder::{ByteOrder, LittleEndian};
use sketch::*;

/// Bit mask for TxE notification.
const TXE_NOTIFICATION: u32 = 1 << 0;
/// Bit mask for RxNE notification.
const RXNE_NOTIFICATION: u32 = 1 << 1;

/// Operation code for read.
const READ_OP: u16 = 0;
/// Operation code for write.
const WRITE_OP: u16 = 1;

/// Response code for "success"
const SUCCESS: u32 = 0;
/// Response code for "unknown operation code"
const UNKNOWN_OP: u32 = 1;
/// Response code for "resources exhausted"
const EXHAUSTED: u32 = 2;

/// Fixed task name for the kernel task
const THE_KERNEL: TaskName = TaskName(0);

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

    // Here's our driver state. We allow a single client at a time to be blocked
    // in each of TX and RX. Any other concurrent requests indicate that people
    // are doing something odd without synchronization, and get declined.
    let mut blocked_txer = None;
    let mut blocked_rxer = None;

    // Main loop.
    loop {
        // Receive any incoming event, either from clients or from
        // notifications. Our largest incoming message is only one byte, so
        // allocate a weeee buffer here.
        let mut buffer = [0; 1];
        let message_info = receive(&mut buffer);

        if message_info.sender == THE_KERNEL {
            // Notification message from the kernel. See which bits were
            // pending.
            let bits = LittleEndian::read_u32(&buffer);

            if bits & TXE_NOTIFICATION != 0 {
                // Transmit holding register has become empty. We can unblock a
                // client.
                if let Some((sender, c)) = blocked_txer.take() {
                    write_thr(c);
                    reply(sender, SUCCESS, &[]);
                } else {
                    // We left TxE enabled without any clients queued? That's a
                    // bug.
                    panic!()
                }

                // Because we only block one client, we know nobody else is
                // pending, so we'll go ahead and shut off the event.
                mask_notifications(TXE_NOTIFICATION);
            }

            if bits & RXNE_NOTIFICATION != 0 {
                // Receive holding register has become non-empty.
                if let Some(sender) = blocked_rxer.take() {
                    reply(sender, SUCCESS, &[read_rbr()]);
                } else {
                    // We left RxNE enabled without any clients queued? That's a
                    // bug.
                    panic!()
                }

                // Because we only block one client, we know nobody else is
                // pending, so we'll go ahead and shut off the event.
                mask_notifications(RXNE_NOTIFICATION);
            }
        } else {
            // Interprocess message from a client
            match message_info.operation {
                READ_OP => {
                    // Read

                    // If the receive holding register is not empty, respond
                    // promptly.
                    if rbr_full() {
                        reply(message_info.sender, SUCCESS, &[read_rbr()]);
                    } else {
                        // Otherwise we need to block the caller.
                        if blocked_rxer.is_none() {
                            blocked_rxer = Some(message_info.sender);
                            // Enable the notification and IRQ. They may already
                            // be enabled; these operations are idempotent and
                            // cheaper than checking.
                            unmask_notifications(RXNE_NOTIFICATION);
                            enable_interrupts(RXNE_NOTIFICATION);
                        } else {
                            // Send back resource exhaustion code.
                            reply(message_info.sender, EXHAUSTED, &[]);
                        }
                    }
                }
                WRITE_OP => {
                    // Write
                    let c = buffer[0];

                    // If the transmit holding register is empty, respond
                    // promptly.
                    if thr_empty() {
                        write_thr(c);
                        reply(message_info.sender, SUCCESS, &[]);
                    } else {
                        // Otherwise we need to block the caller.
                        if blocked_txer.is_none() {
                            blocked_txer = Some((message_info.sender, c));
                            // Enable the notification and IRQ. They may already
                            // be enabled; these operations are idempotent and
                            // cheaper than checking.
                            unmask_notifications(TXE_NOTIFICATION);
                            enable_interrupts(TXE_NOTIFICATION);
                        } else {
                            // Send back resource exhaustion code.
                            reply(message_info.sender, EXHAUSTED, &[]);
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
