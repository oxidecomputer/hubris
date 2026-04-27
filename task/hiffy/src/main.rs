// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! HIF interpreter
//!
//! HIF is the Hubris/Humility Interchange Format, a simple stack-based
//! machine that allows for some dynamic programmability of Hubris.  In
//! particular, this task provides a HIF interpreter to allow for Humility
//! commands like `humility i2c`, `humility pmbus` and `humility jefe`.  The
//! debugger places HIF in [`HIFFY_TEXT`], and then indicates that text is
//! present by incrementing [`HIFFY_KICK`].  This task executes the specified
//! HIF, with the return stack located in [`HIFFY_RSTACK`].

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};
use hif::*;
use static_cell::*;
use userlib::*;

mod common;

cfg_if::cfg_if! {
    if #[cfg(feature = "stm32h7")] {
        pub mod stm32h7;
        use crate::stm32h7::*;
    } else if #[cfg(feature = "lpc55")] {
        pub mod lpc55;
        use crate::lpc55::*;
    } else if #[cfg(feature = "stm32g0")] {
        pub mod stm32g0;
        use crate::stm32g0::*;
    } else if #[cfg(feature = "testsuite")] {
        pub mod tests;
        use crate::tests::*;
    } else {
        pub mod generic;
        use crate::generic::*;
    }
}

#[cfg(all(feature = "turbo", feature = "micro"))]
compile_error!(
    "enabling the 'micro' feature takes precedent over the 'turbo' feature,
     as both control memory buffer size"
);

cfg_if::cfg_if! {
    //
    // The "micro" feature denotes a minimal RAM footprint.  Note that this
    // will restrict Hiffy consumers to functions that return less than 64
    // bytes (minus overhead).  As of this writing, no Idol call returns more
    // than this -- but this will mean that I2cRead functionality that induces
    // a bus or device scan will result in a return stack overflow.
    // (Fortunately, this failure mode is relatively crisp for the consumer.)
    //
    if #[cfg(feature = "micro")] {
        const HIFFY_DATA_SIZE: usize = 256;
        const HIFFY_TEXT_SIZE: usize = 256;
        const HIFFY_RSTACK_SIZE: usize = 64;
        const HIFFY_SCRATCH_SIZE: usize = 512;
    } else if #[cfg(feature = "turbo")] {
        //
        // go-faster mode
        //
        const HIFFY_DATA_SIZE: usize = 20_480;
        const HIFFY_TEXT_SIZE: usize = 4096;
        const HIFFY_RSTACK_SIZE: usize = 2048;

        /// Number of "scratch" bytes available to Hiffy programs. Humility uses this
        /// to deliver data used by some operations. This number can be increased at
        /// the cost of RAM.
        const HIFFY_SCRATCH_SIZE: usize = 1024;
    } else if #[cfg(any(target_board = "donglet-g031", target_board = "oxcon2023g0"))] {
        const HIFFY_DATA_SIZE: usize = 256;
        const HIFFY_TEXT_SIZE: usize = 256;
        const HIFFY_RSTACK_SIZE: usize = 2048;
        const HIFFY_SCRATCH_SIZE: usize = 512;
    } else {
        const HIFFY_DATA_SIZE: usize = 2_048;
        const HIFFY_TEXT_SIZE: usize = 2048;
        const HIFFY_RSTACK_SIZE: usize = 2048;
        const HIFFY_SCRATCH_SIZE: usize = 512;
    }
}

//
// These HIFFY_* global variables constitute the interface with Humility;
// they should not be altered without modifying Humility as well.
//
// - [`HIFFY_TEXT`]       => Program text for HIF operations
// - [`HIFFY_DATA`]       => Binary data from the caller
// - [`HIFFY_RSTACK`]     => HIF return stack
// - [`HIFFY_SCRATCH`]    => Scratch space for hiffy functions; debugger reads
//                           its size but does not modify it
// - [`HIFFY_REQUESTS`]   => Count of succesful requests
// - [`HIFFY_ERRORS`]     => Count of HIF execution failures
// - [`HIFFY_FAILURE`]    => Most recent HIF failure, if any
// - [`HIFFY_KICK`]       => Variable that will be written to to indicate that
//                           [`HIFFY_TEXT`] contains valid program text
// - [`HIFFY_READY`]      => Variable that will be non-zero iff the HIF
//                           execution engine is waiting to be kicked
//
// We are making the following items "no mangle" and "pub" to hint to the
// compiler that they are "exposed", and may be written (spookily) outside the
// scope of Rust itself. The aim is to prevent the optimizer from *assuming* the
// contents of these buffers will remain unchanged between accesses, as they
// will be written directly by the debugger.
//
// Below, we use atomic ordering (e.g. Acquire and Release) to inhibit
// compile- and run-time re-ordering around the explicit sequencing performed
// by the HIFFY_READY, HIFFY_KICK, HIFFY_REQUESTS, and HIFFY_ERRORS that are
// used to arbitrate shared access between the debugger and this software task.
//
// We assume that Hubris and Humility are cooperating, using the following state
// machines to avoid conflicting accesses:
// ┌─────────────────────────────────────────────────────────────────────────────────┐
// │                                                                                 │
// │                                        KICK == 0                                │
// │                    ┌────────────────────────────────────────────────┐           │
// │                    │                                                │           │
// │                    │                                       ┌─────────────────┐  │
// │  ┌─────────┐       ▽  ┌─────────────────┐     ┌───────┐    │ Write READY = 0 │  │
// │  │ Startup │──┬────┴─▷│ Write READY = 1 │────▷│ Sleep │───▷│ Read KICK       │  │
// │  └─────────┘  │       └─────────────────┘     └───────┘    │                 │  │
// │               │                                            └─────────────────┘  │
// │               │                                                     │           │
// │               │  ┌───────────────────────────────┐                  ▽           │
// │               │  │ Read REQUESTS                 │ Success ┌─────────────────┐  │
// │               ├──│ Write REQUESTS = REQUESTS + 1 │◁────┐   │ Write KICK = 0  │  │
// │               │  └───────────────────────────────┘     ├───│ Execute script  │  │
// │               │  ┌───────────────────────────────┐     │   │                 │  │
// │               │  │ Read ERRORS                   │     │   └─────────────────┘  │
// │               └──│ Write ERRORS = ERRORS + 1     │◁────┘                        │
// │                  └───────────────────────────────┘ Failure                      │
// │ ┌────────────┐                                                                  │
// └─┤ Hiffy Task ├──────────────────────────────────────────────────────────────────┘
//   └────────────┘
// ┌─────────────────────────────────────────────────────────────────────────────────┐
// │                                                                                 │
// │                                ┌────────────────┐                ┌────────┐     │
// │          ┌────────┐ READY == 1 │ Read REQUEST   │   REQUEST or   │ Read   │     │
// │        ┌▷│  Idle  │───────────▷│ Read ERRORS    │───────────────▷│ RESULT │     │
// │        │ └────────┘            │ Write KICK = 1 │ ERRORS changed │        │     │
// │        │                       └────────────────┘                └────────┘     │
// │        │                                                              │         │
// │        └──────────────────────────────────────────────────────────────┘         │
// │ ┌──────────┐                                                                    │
// └─┤ Humility ├────────────────────────────────────────────────────────────────────┘
//   └──────────┘
#[unsafe(no_mangle)]
pub static mut HIFFY_TEXT: [u8; HIFFY_TEXT_SIZE] = [0; HIFFY_TEXT_SIZE];
#[unsafe(no_mangle)]
pub static mut HIFFY_DATA: [u8; HIFFY_DATA_SIZE] = [0; HIFFY_DATA_SIZE];
#[unsafe(no_mangle)]
pub static mut HIFFY_RSTACK: [u8; HIFFY_RSTACK_SIZE] = [0; HIFFY_RSTACK_SIZE];

pub static HIFFY_SCRATCH: StaticCell<[u8; HIFFY_SCRATCH_SIZE]> =
    StaticCell::new([0; HIFFY_SCRATCH_SIZE]);

#[unsafe(no_mangle)]
pub static HIFFY_REQUESTS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static HIFFY_ERRORS: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static HIFFY_KICK: AtomicU32 = AtomicU32::new(0);
#[unsafe(no_mangle)]
pub static HIFFY_READY: AtomicU32 = AtomicU32::new(0);

#[unsafe(no_mangle)]
pub static mut HIFFY_FAILURE: Option<Failure> = None;

// We deliberately export the HIF version numbers to allow Humility to
// fail cleanly if its HIF version does not match our own.
//
// Note that `#[unsafe(no_mangle)]` does not preserve these values through the
// linker, so we used `#[used]` instead.  They are not used by any code, so
// there's no safety concerns.
#[used]
pub static HIFFY_VERSION_MAJOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MAJOR);
#[used]
pub static HIFFY_VERSION_MINOR: AtomicU32 = AtomicU32::new(HIF_VERSION_MINOR);
#[used]
pub static HIFFY_VERSION_PATCH: AtomicU32 = AtomicU32::new(HIF_VERSION_PATCH);

#[unsafe(export_name = "main")]
fn main() -> ! {
    let mut sleep_ms = 250;
    let mut sleeps = 0;
    let mut stack = [None; 32];
    const NLABELS: usize = 4;

    #[cfg(feature = "net")]
    let mut net_state = net::State::new();

    loop {
        // Sleep until either the timer expires or we receive a notification
        // from the `net` task indicating that it's ready for us.
        let deadline = sys_get_timer().now.saturating_add(sleep_ms);
        HIFFY_READY.store(1, Ordering::Relaxed);
        sys_set_timer(Some(deadline), notifications::TIMER_MASK);

        #[cfg(feature = "net")]
        let bits = notifications::SOCKET_MASK | notifications::TIMER_MASK;
        #[cfg(not(feature = "net"))]
        let bits = notifications::TIMER_MASK;

        let notif = sys_recv_notification(bits);
        HIFFY_READY.store(0, Ordering::Relaxed);

        #[cfg(feature = "net")]
        if notif.check_notification_mask(notifications::SOCKET_MASK) {
            net_state.check_net();
        }

        if notif.has_timer_fired(notifications::TIMER_MASK) {
            // Humility writes `1` to `HIFFY_KICK`
            if HIFFY_KICK.load(Ordering::Acquire) == 0 {
                sleeps += 1;

                // Exponentially backoff our sleep value, but no more than 250ms
                if sleeps == 10 {
                    sleep_ms = core::cmp::min(sleep_ms * 10, 250);
                    sleeps = 0;
                }

                continue;
            }

            //
            // Whenever we have been kicked, we adjust our timeout down to 1ms,
            // from which we will exponentially backoff
            //
            HIFFY_KICK.store(0, Ordering::Release);
            sleep_ms = 1;
            sleeps = 0;

            let check = |offset: usize, op: &Op| -> Result<(), Failure> {
                trace_execute(offset, *op);
                Ok(())
            };
            let rv = {
                // Dummy object to bind references to a non-static lifetime
                let lifetime = ();

                // SAFETY: We construct references from our pointers with a limited
                // (non-static) lifetime, so they can't escape this block.  We are
                // in single-threaded code, so no one else can read or write to
                // static memory.  While the HIF program is running, the debugger is
                // only reading from `HIFFY_REQUESTS` and `HIFFY_ERRORS`; it is not
                // writing to any locations in memory.  See the diagram above for
                // Hubris / Humility coordination.
                let (text, data, rstack) = unsafe {
                    (
                        bind_lifetime_ref(&lifetime, &raw const HIFFY_TEXT),
                        bind_lifetime_ref(&lifetime, &raw const HIFFY_DATA),
                        bind_lifetime_mut(&lifetime, &raw mut HIFFY_RSTACK),
                    )
                };
                execute::<_, NLABELS>(
                    text,
                    HIFFY_FUNCS,
                    data,
                    &mut stack,
                    rstack,
                    &mut *HIFFY_SCRATCH.borrow_mut(),
                    check,
                )
            };

            match rv {
                Ok(_) => {
                    let prev = HIFFY_REQUESTS.load(Ordering::Relaxed);
                    HIFFY_REQUESTS
                        .store(prev.wrapping_add(1), Ordering::Release);
                    trace_success();
                }
                Err(failure) => {
                    // SAFETY: We are in single-threaded code and the debugger will
                    // not be reading HIFFY_FAILURE until HIFFY_ERRORS is
                    // incremented below.  See the diagram above for Hubris /
                    // Humility coordination.
                    unsafe {
                        HIFFY_FAILURE = Some(failure);
                        let prev = HIFFY_ERRORS.load(Ordering::Relaxed);
                        HIFFY_ERRORS
                            .store(prev.wrapping_add(1), Ordering::Release);
                        trace_failure(failure);
                    }
                }
            }
        }
    }
}

/// Converts an array pointer to a shared reference with a particular lifetime
///
/// # Safety
/// `ptr` must point to a valid, aligned, initialized `[u8; N]`.
/// The referent must not be mutated while the returned reference is live.
#[expect(clippy::needless_lifetimes)] // gotta make it obvious
unsafe fn bind_lifetime_ref<'a, const N: usize>(
    _: &'a (),
    array: *const [u8; N],
) -> &'a [u8; N] {
    // SAFETY: converting from pointer to reference is safe given the function's
    // safety conditions (listed in docstring)
    unsafe { array.as_ref().unwrap_lite() }
}

/// Converts an array pointer to a mutable reference with a particular lifetime
///
/// # Safety
/// `ptr` must point to a valid, aligned, initialized `[u8; N]`.
/// The referent must not be mutated while the returned reference is live.
#[expect(clippy::needless_lifetimes, clippy::mut_from_ref)]
unsafe fn bind_lifetime_mut<'a, const N: usize>(
    _: &'a (),
    array: *mut [u8; N],
) -> &'a mut [u8; N] {
    // SAFETY: converting from pointer to reference is safe given the function's
    // safety conditions (listed in docstring)
    unsafe { array.as_mut().unwrap_lite() }
}

#[cfg(feature = "net")]
mod net {
    use super::{
        HIFFY_DATA, HIFFY_KICK, HIFFY_TEXT, bind_lifetime_mut, notifications,
    };
    use core::sync::atomic::Ordering;
    use static_cell::ClaimOnceCell;
    use task_net_api::{
        LargePayloadBehavior, RecvError, SendError, SocketName, UdpMetadata,
    };
    use userlib::{FromPrimitive, UnwrapLite, sys_recv_notification};
    use zerocopy::{FromBytes, IntoBytes, LittleEndian, U16, U32, U64};

    const SOCKET: SocketName = SocketName::hiffy;
    const SOCKET_TX_SIZE: usize = task_net_api::SOCKET_TX_SIZE[SOCKET as usize];
    const SOCKET_RX_SIZE: usize = task_net_api::SOCKET_RX_SIZE[SOCKET as usize];

    /// Header for an RPC request
    ///
    /// `humility` must cooperate with this layout and the `OP_*` values below;
    /// they are mirrored in `doppel.rs`.
    #[derive(Copy, Clone, Debug, FromBytes)]
    #[repr(C)]
    struct RpcHeader {
        /// Expected image ID
        image_id: U64<LittleEndian>,
        /// Header version (always 1 right now)
        version: U16<LittleEndian>,
        /// Operation to perform
        operation: U16<LittleEndian>,
        /// Argument-dependent operation
        arg: U32<LittleEndian>,
    }
    const CURRENT_VERSION: u16 = 1;

    #[derive(Copy, Clone, Debug, FromPrimitive)]
    #[repr(u16)]
    enum RpcOp {
        WriteHiffyText = 1,
        WriteHiffyData,
        HiffyKick,
    }

    #[derive(Copy, Clone, Debug)]
    #[repr(u8)]
    enum RpcReply {
        Ok = 0u8,
        /// The RPC packet was too short to include the complete header
        TooShort,
        /// The RPC packet's image ID does not match ours
        BadImageId,
        /// The RPC packet's header version does not match our version
        BadVersion,
        /// The RPC operation field is invalid
        InvalidOperation,
        /// The write exceeds our data buffers
        OutOfRange,
    }

    userlib::task_slot!(NET, net);

    pub(super) struct State {
        net: task_net_api::Net,
        tx_data_buf: &'static mut [u8],
        rx_data_buf: &'static mut [u8],
        image_id: u64,
    }
    impl State {
        pub(super) fn new() -> Self {
            let (tx_data_buf, rx_data_buf) = {
                static BUFS: ClaimOnceCell<(
                    [u8; SOCKET_TX_SIZE],
                    [u8; SOCKET_RX_SIZE],
                )> = ClaimOnceCell::new((
                    [0; SOCKET_TX_SIZE],
                    [0; SOCKET_RX_SIZE],
                ));
                BUFS.claim()
            };
            let net = task_net_api::Net::from(NET.get_task_id());
            let image_id = userlib::kipc::read_image_id();
            Self {
                net,
                tx_data_buf,
                rx_data_buf,
                image_id,
            }
        }
        pub(super) fn check_net(&mut self) {
            match self.net.recv_packet(
                SOCKET,
                LargePayloadBehavior::Discard,
                self.rx_data_buf,
            ) {
                Ok(meta) => self.handle_packet(meta),
                Err(RecvError::QueueEmpty | RecvError::ServerRestarted) => {
                    // Our incoming queue is empty or `net` restarted. Wait for
                    // more packets in dispatch, back in the main loop.
                }
            }
        }

        fn handle_packet(&mut self, mut meta: UdpMetadata) {
            // Steal `tx_data_buf` to work around lifetime shenanigans;
            // `handle_packet_inner` does not write to it!
            let tx_data_buf = core::mem::take(&mut self.tx_data_buf);
            let (r, data) = self.handle_packet_inner(meta);
            tx_data_buf[0] = r as u8;
            tx_data_buf[1..][..data.len()].copy_from_slice(data);
            meta.size = (1 + data.len()) as u32;
            self.tx_data_buf = tx_data_buf;
            loop {
                match self.net.send_packet(
                    SOCKET,
                    meta,
                    &self.tx_data_buf[..(meta.size as usize)],
                ) {
                    Ok(()) => break,
                    // If `net` just restarted, immediately retry our send.
                    Err(SendError::ServerRestarted) => continue,
                    // If our tx queue is full, wait for space. This is the
                    // same notification we get for incoming packets, so we
                    // might spuriously wake up due to an incoming packet
                    // (which we can't service anyway because we are still
                    // waiting to respond to a previous request); once we
                    // finally succeed in sending we'll peel any queued
                    // packets off our recv queue at the top of our main
                    // loop.
                    Err(SendError::QueueFull) => {
                        sys_recv_notification(notifications::SOCKET_MASK);
                    }
                }
            }
        }

        fn handle_packet_inner(&self, meta: UdpMetadata) -> (RpcReply, &[u8]) {
            const HEADER_SIZE: usize = core::mem::size_of::<RpcHeader>();
            if (meta.size as usize) < HEADER_SIZE {
                return (RpcReply::TooShort, &[]);
            }

            // We can always read the header, since it's raw data
            let header =
                RpcHeader::read_from_bytes(&self.rx_data_buf[..HEADER_SIZE])
                    .unwrap_lite();
            let rest = &self.rx_data_buf[HEADER_SIZE..];
            if self.image_id != header.image_id.get() {
                return (RpcReply::BadImageId, self.image_id.as_bytes());
            }

            if header.version.get() != 1 {
                return (RpcReply::BadVersion, CURRENT_VERSION.as_bytes());
            }

            // Perform the actual operation
            match RpcOp::from_u16(header.operation.get()) {
                Some(RpcOp::WriteHiffyText) => {
                    // Dummy object to bind references to a non-static lifetime
                    let lifetime = ();
                    let offset = header.arg.get() as usize;

                    // SAFETY: we are constructing a slice with a bounded
                    // lifetime, and are in single-threaded code.  We don't
                    // expect a debugger to be editing our memory.  If someone
                    // is simultaneously editing `HIFFY_TEXT` with a debugger
                    // *and* over the network, they deserve whatever happens.
                    let text = unsafe {
                        bind_lifetime_mut(&lifetime, &raw mut HIFFY_TEXT)
                    };
                    if let Some(chunk) = offset
                        .checked_add(rest.len())
                        .and_then(|e| text.get_mut(offset..e))
                    {
                        chunk.copy_from_slice(rest);
                        (RpcReply::Ok, &[])
                    } else {
                        (RpcReply::OutOfRange, &[])
                    }
                }
                Some(RpcOp::WriteHiffyData) => {
                    // Dummy object to bind references to a non-static lifetime
                    let lifetime = ();
                    let offset = header.arg.get() as usize;

                    // SAFETY: we are constructing a slice with a bounded
                    // lifetime, and are in single-threaded code.  We don't
                    // expect a debugger to be editing our memory.  If someone
                    // is simultaneously editing `HIFFY_DATA` with a debugger
                    // *and* over the network, they deserve whatever happens.
                    let data = unsafe {
                        bind_lifetime_mut(&lifetime, &raw mut HIFFY_DATA)
                    };
                    if let Some(chunk) = offset
                        .checked_add(rest.len())
                        .and_then(|e| data.get_mut(offset..e))
                    {
                        chunk.copy_from_slice(rest);
                        (RpcReply::Ok, &[])
                    } else {
                        (RpcReply::OutOfRange, &[])
                    }
                }
                Some(RpcOp::HiffyKick) => {
                    HIFFY_KICK.fetch_add(1, Ordering::SeqCst);
                    (RpcReply::Ok, &[])
                }
                None => (RpcReply::InvalidOperation, &[]),
            }
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
