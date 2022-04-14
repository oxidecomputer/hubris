// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Test suite.
//!
//! This task is driven by the `runner` to run test cases (defined below).
//!
//! Any test case that fails should indicate this by `panic!` (or equivalent,
//! like failing an `assert!`).
//!
//! # The assistant
//!
//! This test suite uses two other tasks to test IPC and interactions.
//!
//! The assistant, `test-assist`, tests raw test IPC and interactions.  It must
//! be included in the image with the name `assist`, but its ID is immaterial.
//!
//! The Idol server, `test-idol-server`, tests Idol-mediated IPC.  It must be
//! included in the image with the name `idol`, but its ID is immaterial.

#![no_std]
#![no_main]
#![feature(asm)]

use hubris_num_tasks::NUM_TASKS;
use test_api::*;
use userlib::*;
use zerocopy::AsBytes;

/// Helper macro for building a list of functions with their names.
macro_rules! test_cases {
    ($($(#[$attr:meta])* $name:path,)*) => {
        static TESTS: &[(&str, &(dyn Fn() + Send + Sync))] = &[
            $(
                $(#[$attr])*
                (stringify!($name), &$name)
            ),*
        ];
    };
}

// Test the `task_config!` macro, in cooperation with `test_task_config` below
// and the `[tests.suite.config]` block in the `app.toml` file.
task_config::task_config! {
    foo: &'static str,
    bar: u32,
    baz: &'static [u8],
    tup: &'static [(u32, bool)],
}

// Actual list of functions with their names.
test_cases! {
    test_send,
    test_recv_reply,
    test_recv_reply_fault,
    #[cfg(any(armv7m, armv8m))]
    test_floating_point_lowregs,
    #[cfg(any(armv7m, armv8m))]
    test_floating_point_highregs,
    #[cfg(any(armv7m, armv8m))]
    test_floating_point_fault,
    test_fault_badmem,
    test_fault_stackoverflow,
    test_fault_execdata,
    test_fault_illop,
    test_fault_nullexec,
    test_fault_textoob,
    test_fault_stackoob,
    test_fault_buserror,
    test_fault_illinst,
    #[cfg(any(armv7m, armv8m))]
    test_fault_divzero,
    test_fault_maxstatus,
    test_fault_badstatus,
    test_fault_maxrestart,
    test_fault_badrestart,
    test_fault_maxinjection,
    test_fault_badinjection,
    test_fault_superinjection,
    test_fault_selfinjection,
    test_panic,
    test_restart,
    test_restart_taskgen,
    test_borrow_info,
    test_borrow_read,
    test_borrow_write,
    test_borrow_without_peer_waiting,
    test_supervisor_fault_notification,
    test_timer_advance,
    test_timer_notify,
    test_timer_notify_past,
    test_task_config,
    test_task_status,
    test_task_fault_injection,
    test_refresh_task_id_basic,
    test_refresh_task_id_off_by_one,
    test_refresh_task_id_off_by_many,
    test_post,
    test_idol_basic,
    test_idol_bool_arg,
    test_idol_bool_ret,
    test_idol_bool_xor,
    test_idol_err_ret,
    test_idol_ssmarshal,
    test_idol_ssmarshal_multiarg,
    test_idol_ssmarshal_multiarg_enum,
    #[cfg(feature = "fru-id-eeprom")]
    at24csw080::test_at24csw080,
}

/// Tests that we can send a message to our assistant, and that the assistant
/// can reply. Technically this is also a test of RECV/REPLY on the assistant
/// side but hey.
fn test_send() {
    let assist = assist_task_id();
    let challenge = 0xDEADBEEF_u32;
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::JustReply as u16,
        &challenge.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    assert_eq!(response, !0xDEADBEEF);
}

/// Tests that we can receive a message from the assistant and reply.
fn test_recv_reply() {
    let assist = assist_task_id();

    // Ask the assistant to send us a message containing this challenge value.
    let challenge = 0xCAFE_F00Du32;
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::SendBack as u16,
        &challenge.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    // Don't actually care about the response in this case

    // Switch roles and wait for the message, blocking notifications.
    let rm = sys_recv_open(response.as_bytes_mut(), 0);
    assert_eq!(rm.sender, assist);
    assert_eq!(rm.operation, 42); // assistant always sends this

    // Check that we got the expected challenge back.
    assert_eq!(rm.message_len, 4);
    assert_eq!(response, challenge);

    // Check that the other message attributes seem legit.
    assert_eq!(rm.response_capacity, 4);
    assert_eq!(rm.lease_count, 0);

    // Send a recognizeable value in our reply; the assistant will record it.
    let reply_token = 0x1DE_u32;
    sys_reply(assist, 0, &reply_token.to_le_bytes());

    // Call back to the assistant and request a copy of our most recent reply.
    let (rc, len) = sys_send(
        assist,
        AssistOp::LastReply as u16,
        &challenge.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    assert_eq!(response, reply_token);
}

/// Tests that we can receive a message from the assistant and then fault it.
fn test_recv_reply_fault() {
    let assist = assist_task_id();

    // Ask the assistant to send us a message containing this challenge value.
    let challenge = 0xCAFE_F00Du32;
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::SendBack as u16,
        &challenge.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    // Now take the message. This is necessary to be able to fault the task.
    let _rm = sys_recv_open(response.as_bytes_mut(), 0);

    // We don't validate the message itself because the test_recv_reply above
    // covers that. We're specifically interested in what happens if we...
    sys_reply_fault(assist, ReplyFaultReason::AccessViolation);

    // Ask the kernel to report the assistant's state.
    let status = kipc::read_task_status(ASSIST.get_task_index().into());
    let this_task = TaskId::for_index_and_gen(1, Generation::default());
    let this_task = sys_refresh_task_id(this_task);

    match status {
        TaskState::Faulted { fault, .. } => {
            assert_eq!(
                fault,
                FaultInfo::FromServer(
                    this_task,
                    ReplyFaultReason::AccessViolation
                )
            );
        }
        _ => {
            panic!("expected fault");
        }
    }
}

/// Helper routine to send a message to the assistant telling it to fault,
/// and then verifying that the fault caused a state change into the `Faulted`
/// state, returning the actual fault info.
fn test_fault(op: AssistOp, arg: u32) -> FaultInfo {
    let assist = assist_task_id();

    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        op as u16,
        &arg.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    // Don't actually care about the response in this case

    // Ask the kernel to report the assistant's state.
    let status = kipc::read_task_status(ASSIST.get_task_index().into());

    match status {
        TaskState::Faulted {
            fault,
            original_state,
        } => {
            assert_eq!(original_state, SchedState::Runnable);
            fault
        }
        _ => {
            panic!("expected fault");
        }
    }
}

cfg_if::cfg_if! {
    if #[cfg(armv6m)] {
        macro_rules! assert_fault_eq {
            ($name:expr, $expected:expr) => {
                assert_eq!($name, FaultInfo::InvalidOperation(0));
            };
        }
    } else {
        macro_rules! assert_fault_eq {
            ($name:expr, $expected:expr) => {
                assert_eq!($name, $expected);
            };
        }
    }
}

/// Tests a memory fault, which ensures that the address reporting is correct,
/// and that the MPU is on.
fn test_fault_badmem() {
    let bad_address = BAD_ADDRESS;
    let fault = test_fault(AssistOp::BadMemory, bad_address);

    assert_fault_eq!(
        fault,
        FaultInfo::MemoryAccess {
            address: Some(bad_address),
            source: FaultSource::User,
        }
    );
}

fn test_fault_stackoverflow() {
    let fault = test_fault(AssistOp::StackOverflow, 0);

    match fault {
        FaultInfo::StackOverflow { .. } => {}
        #[cfg(armv6m)]
        FaultInfo::InvalidOperation(_) => {}
        _ => {
            panic!("expected StackOverflow; found {:?}", fault);
        }
    }
}

fn test_fault_execdata() {
    assert_fault_eq!(test_fault(AssistOp::ExecData, 0), FaultInfo::IllegalText);
}

fn test_fault_illop() {
    let fault = test_fault(AssistOp::IllegalOperation, 0);

    match fault {
        FaultInfo::InvalidOperation { .. } => {}
        _ => {
            panic!("expected InvalidOperation; found {:?}", fault);
        }
    }
}

fn test_fault_nullexec() {
    assert_fault_eq!(
        test_fault(AssistOp::BadExec, BAD_ADDRESS),
        FaultInfo::IllegalText
    );
}

fn test_fault_textoob() {
    let fault = test_fault(AssistOp::TextOutOfBounds, BAD_ADDRESS);

    match fault {
        FaultInfo::BusError { .. } | FaultInfo::MemoryAccess { .. } => {}
        #[cfg(armv6m)]
        FaultInfo::InvalidOperation(_) => {}
        _ => {
            panic!("expected BusFault or MemoryAccess; found {:?}", fault);
        }
    }
}

fn test_fault_stackoob() {
    let fault = test_fault(AssistOp::StackOutOfBounds, 0);
    match fault {
        FaultInfo::MemoryAccess { .. } => {}
        #[cfg(armv6m)]
        FaultInfo::InvalidOperation(_) => {}
        _ => {
            panic!("expected MemoryAccess; found {:?}", fault);
        }
    }
}

fn test_fault_buserror() {
    let fault = test_fault(AssistOp::BusError, 0);

    match fault {
        FaultInfo::BusError { .. } => {}
        #[cfg(armv6m)]
        FaultInfo::InvalidOperation(_) => {}
        _ => {
            panic!("expected BusFault; found {:?}", fault);
        }
    }
}

fn test_fault_illinst() {
    assert_fault_eq!(
        test_fault(AssistOp::IllegalInstruction, 0),
        FaultInfo::IllegalInstruction
    );
}

/// Tests that division-by-zero results in a DivideByZero fault
#[cfg(any(armv7m, armv8m))]
fn test_fault_divzero() {
    assert_fault_eq!(test_fault(AssistOp::DivZero, 0), FaultInfo::DivideByZero);
}

fn test_fault_badtaskop(op: AssistOp, id: usize) {
    match op {
        AssistOp::ReadTaskStatus
        | AssistOp::FaultTask
        | AssistOp::RestartTask => {}
        _ => {
            panic!("illegal task operation");
        }
    }

    assert_eq!(
        test_fault(op, id as u32),
        FaultInfo::SyscallUsage(UsageError::TaskOutOfRange)
    );
}

fn test_fault_maxstatus() {
    test_fault_badtaskop(AssistOp::ReadTaskStatus, usize::MAX);
}

fn test_fault_badstatus() {
    test_fault_badtaskop(AssistOp::ReadTaskStatus, NUM_TASKS);
}

fn test_fault_maxrestart() {
    test_fault_badtaskop(AssistOp::RestartTask, usize::MAX);
}

fn test_fault_badrestart() {
    test_fault_badtaskop(AssistOp::RestartTask, NUM_TASKS);
}

fn test_fault_maxinjection() {
    test_fault_badtaskop(AssistOp::FaultTask, usize::MAX);
}

fn test_fault_badinjection() {
    test_fault_badtaskop(AssistOp::FaultTask, NUM_TASKS);
}

fn test_fault_superinjection() {
    assert_eq!(
        test_fault(AssistOp::FaultTask, 0),
        FaultInfo::SyscallUsage(UsageError::IllegalTask)
    );
}

fn test_fault_selfinjection() {
    assert_eq!(
        test_fault(AssistOp::FaultTask, ASSIST.get_task_index().into()),
        FaultInfo::SyscallUsage(UsageError::IllegalTask)
    );
}

/// Tests that a `panic!` in a task is recorded as a fault.
fn test_panic() {
    let assist = assist_task_id();

    // Ask the assistant to panic.
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::Panic as u16,
        &0u32.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    // Don't actually care about the response in this case

    // Read status back from the kernel and check it.
    let status = kipc::read_task_status(ASSIST.get_task_index().into());
    assert_eq!(
        status,
        TaskState::Faulted {
            fault: FaultInfo::Panic,
            original_state: SchedState::Runnable,
        },
    );
    restart_assistant();
}

fn test_idol_basic() {
    let idol = idol_handle();
    let r = idol.increment(1);
    assert_eq!(r, Ok(2));
}

fn test_idol_bool_arg() {
    let idol = idol_handle();
    let r = idol.maybe_increment(5, true);
    assert_eq!(r, Ok(6));

    let r = idol.maybe_increment(5, false);
    assert_eq!(r, Ok(5));
}

fn test_idol_bool_ret() {
    let idol = idol_handle();
    let r = idol.bool_not(true);
    assert_eq!(r, Ok(false));

    let r = idol.bool_not(false);
    assert_eq!(r, Ok(true));
}

fn test_idol_bool_xor() {
    let idol = idol_handle();
    let r = idol.bool_xor(true, true);
    assert_eq!(r, Ok(false));

    let r = idol.bool_xor(true, false);
    assert_eq!(r, Ok(true));

    let r = idol.bool_xor(false, true);
    assert_eq!(r, Ok(true));

    let r = idol.bool_xor(false, false);
    assert_eq!(r, Ok(false));
}

fn test_idol_err_ret() {
    let idol = idol_handle();
    let r = idol.return_err_if_true(false);
    assert_eq!(r, Ok(()));
    let r = idol.return_err_if_true(true);
    assert_eq!(r, Err(test_idol_api::IdolTestError::YouAskedForThis));
}

fn test_idol_ssmarshal() {
    let idol = idol_handle();
    let r = idol
        .fancy_increment(test_idol_api::FancyTestType {
            u: 133,
            b: true,
            f: 1.0,
        })
        .unwrap();

    // We deliberately avoid using assert_eq! for float comparison, because
    // it brings in 14K of float formatting code and overflows our smaller
    // targets.
    assert_eq!(r.u, 134);
    assert_eq!(r.b, true);
    assert!(r.f == 1.0);

    let r = idol
        .fancy_increment(test_idol_api::FancyTestType {
            u: 101,
            b: false,
            f: 1.0,
        })
        .unwrap();
    assert_eq!(r.u, 101);
    assert_eq!(r.b, false);
    assert!(r.f == 1.0);
}

fn test_idol_ssmarshal_multiarg() {
    use test_idol_api::*;
    let idol = idol_handle();
    let r = idol
        .extract_vid(
            0xAA,
            UdpMetadata {
                addr: Address::Ipv6(Ipv6Address([
                    1, 2, 3, 4, 5, 6, 7, 8, 1, 1, 2, 2, 3, 3, 4, 4,
                ])),
                port: 1021,
                size: 1,
                vid: 12,
            },
        )
        .unwrap();

    assert_eq!(r, 12);
}

fn test_idol_ssmarshal_multiarg_enum() {
    use test_idol_api::*;
    let idol = idol_handle();
    let r = idol
        .extract_vid_enum(
            SocketName::Echo,
            UdpMetadata {
                addr: Address::Ipv6(Ipv6Address([
                    1, 2, 3, 4, 5, 6, 7, 8, 1, 1, 2, 2, 3, 3, 4, 4,
                ])),
                port: 1021,
                size: 1,
                vid: 14,
            },
        )
        .unwrap();

    assert_eq!(r, 14);
}

#[cfg(feature = "i2c-devices")]
include!(concat!(env!("OUT_DIR"), "/i2c_config.rs"));

#[cfg(feature = "i2c-devices")]
task_slot!(I2C, i2c_driver);

// Put the FRU ID tests into their own module, so it can be enabled with
// a single cfg block
#[cfg(feature = "fru-id-eeprom")]
mod at24csw080 {
    use super::*;
    use drv_i2c_devices::at24csw080::Error;
    use drv_i2c_devices::at24csw080::*;

    const EEPROM_SIZE: u16 = 1024;
    const PAGE_SIZE: u16 = 16;

    pub(super) fn test_at24csw080() {
        let i2c_task = I2C.get_task_id();
        let dev =
            At24csw080::new(i2c_config::devices::at24csw080_local(i2c_task)[0]);

        // The FRU ID EEPROM is 1KByte.  Because we want to exercise the
        // ability to rewrite the EEPROM (and have that persist through power
        // loss), we define two different patterns based on the first byte.
        //
        // Depending on the value of the first byte in the EEPROM, we make the
        // following choices:
        //
        //  If the byte is 0x0A, indicating pattern A
        //      Write the byte to 0xFF, indicating that the pattern is empty
        //      Validate pattern A (ignoring the first byte)
        //      Write pattern B (leaving the first byte at 0xFF)
        //      Validate pattern B (ignoring the first byte)
        //      Write the first byte to 0x0B
        //  If the byte is 0x0B, indicating pattern B
        //      Write the byte to 0xFF, indicating that the pattern is empty
        //      Validate pattern B (ignoring the first byte)
        //      Write pattern A (leaving the first byte at 0xFF)
        //      Validate pattern A (ignoring the first byte)
        //      Write the first byte to 0x0A
        //  Otherwise, the EEPROM is empty or has an invalid pattern
        //      Write pattern A (leaving the first byte at 0xFF)
        //      Validate the A pattern
        //      Write the first byte to 0x0A
        //
        //  Notice that we clear the first byte of the EEPROM _before_ doing
        //  validation. This ensures that if validation fails, the EEPROM is
        //  left in a state where it falls back to the default case, instead
        //  of getting stuck.
        let seed = dev.read::<u8>(0).unwrap();
        match seed {
            0x0A => {
                dev.write_byte(0, 0xFF).unwrap();
                validate_eeprom(&dev, 0x0A).unwrap();
                test_eeprom(&dev, 0x0B).unwrap();
                validate_eeprom(&dev, 0x0B).unwrap();
                dev.write(0, 0x0Bu8).unwrap();
            }
            0x0B => {
                dev.write_byte(0, 0xFF).unwrap();
                validate_eeprom(&dev, 0x0B).unwrap();
                test_eeprom(&dev, 0x0A).unwrap();
                validate_eeprom(&dev, 0x0A).unwrap();
                dev.write(0, 0x0A_u8).unwrap();
            }
            _ => {
                test_eeprom(&dev, 0x0A).unwrap();
                validate_eeprom(&dev, 0x0A).unwrap();
                dev.write(0, 0x0A_u8).unwrap();
            }
        }
        sys_log!("Completed EEPROM test with seed {}", seed);
    }

    /// Simple maximal LFSR to generate a stream of pseudo-random bytes
    fn next(i: &mut u8) -> u8 {
        *i = (*i << 1) | ((*i & 0b10110100).count_ones() as u8 & 1);
        *i
    }

    fn test_eeprom(dev: &At24csw080, seed: u8) -> Result<(), Error> {
        assert!(seed != 0);

        // If the security register is (permanently) locked, then we won't be
        // able to run part of this test, so fail early.
        assert!(!dev.is_security_register_locked()?);

        // If the write protection register is locked, then we're going to have
        // a bad time, so fail early as well.
        //
        // It's possible for the write protection register to be (temporarily)
        // enabled if someone pulls power to the EEPROM at just the right point
        // in the test suite; in that case, we disable write protection but
        // don't fail the test suite.
        let wpr = dev.read_eeprom_write_protect()?;
        assert!(!wpr.locked);
        if wpr.block.is_some() {
            dev.disable_eeprom_write_protection()?;
        }

        // Test error handling in the driver itself by doing a bunch of
        // operations that should be invalid
        //
        // There are a few errors that we can't easily test
        // - InvalidObjectSize requires a single object that's > 64K, which is
        //   unlikely in our embedded system.
        // - MisalignedPage and InvalidPageSize are both only generated by
        //   a private function (write_page)
        assert_eq!(
            dev.write(1028, 0x1234_u32),
            Err(Error::InvalidAddress(1028))
        );
        assert_eq!(
            dev.write(1022, 0x1234_u32),
            Err(Error::InvalidEndAddress(1026))
        );
        assert_eq!(
            dev.write_security_register_byte(0, 1),
            Err(Error::InvalidSecurityRegisterWriteByte(0))
        );
        assert_eq!(
            dev.write_security_register_byte(33, 1),
            Err(Error::InvalidSecurityRegisterWriteByte(33))
        );
        assert_eq!(
            dev.read_security_register_byte(33),
            Err(Error::InvalidSecurityRegisterReadByte(33))
        );

        // Write random bytes unevenly spaced through memory, then read them
        // back and check their values.
        let mut i = seed;
        for addr in (PAGE_SIZE..EEPROM_SIZE).step_by(PAGE_SIZE as usize + 1) {
            dev.write(addr, next(&mut i))?;
        }
        let mut i = seed;
        for addr in (PAGE_SIZE..EEPROM_SIZE).step_by(PAGE_SIZE as usize + 1) {
            assert_eq!(dev.read::<u8>(addr)?, next(&mut i));
        }

        // Generate a pseudo-random buffer, which we'll write in various places
        const BUF_SIZE: u16 = 31;
        type BufType = [u8; BUF_SIZE as usize];
        let buf = {
            let mut buf: BufType = BufType::default();
            let mut i = seed;
            buf.iter_mut().for_each(|b| *b = next(&mut i));
            buf
        };

        // Write the buffer in a variety of page-straddling locations then
        // read it back and look for issues.
        for addr in [69, 254, 510] {
            dev.write(addr, buf)?;
            assert!(dev.read::<BufType>(addr)? == buf);
            dev.write(addr + BUF_SIZE, buf)?;
            assert!(dev.read::<BufType>(addr)? == buf);
            assert!(dev.read::<BufType>(addr + BUF_SIZE)? == buf);
            dev.write(addr - BUF_SIZE, buf)?;
            assert!(dev.read::<BufType>(addr - BUF_SIZE)? == buf);
            assert!(dev.read::<BufType>(addr + BUF_SIZE)? == buf);
            assert!(dev.read::<BufType>(addr)? == buf);
        }

        // Write the upper 16 bytes of the security register based on the lower
        // 16 bytes and the seed, then read back values to test.  We'll read
        // back these values in `validate_eeprom`, since this should be
        // persistent through power cycles.
        let mut h = seed;
        for i in 0..16 {
            let v = dev.read_security_register_byte(i)? ^ next(&mut h);
            dev.write_security_register_byte(i + 16, v)?;
        }

        // Make sure that the EEPROM write protection register works
        // (only enabling it temporarily, do not fear)
        let addr = 760;
        dev.write(addr, buf)?;
        assert!(dev.read::<BufType>(addr)? == buf);
        dev.enable_eeprom_write_protection(WriteProtectBlock::Upper256Bytes)?;
        assert!(dev.read::<BufType>(addr)? == buf);
        dev.write(addr, BufType::default())?;
        // At this point, we expect the bottom 8 bytes to be cleared (because
        // they're in the unprotected region), and the remaining bytes to be
        // the same as before
        let v = dev.read::<BufType>(addr)?;
        assert!(v[0..8] == [0; 8]);
        assert!(v[8..] == buf[8..]);
        // Now disable write protection; clear that chunk of memory, and
        // confirm that the clearing worked on the entire buffer.
        dev.disable_eeprom_write_protection()?;
        dev.write(addr, BufType::default())?;
        assert!(dev.read::<BufType>(addr)? == BufType::default());

        // To finish, write most of the EEPROM with a pseudorandom pattern.
        // We'll check this in `validate_eeprom` to make sure it persists
        // through power-off.
        let mut i = seed;
        for addr in (PAGE_SIZE..EEPROM_SIZE).step_by(PAGE_SIZE as usize) {
            // Using page-size writes to minimize the number of 5ms waits
            let mut buf = [0u8; PAGE_SIZE as usize];
            buf.iter_mut().for_each(|b| *b = next(&mut i));
            dev.write(addr, buf)?;
        }

        // Fill the first page with the seed value, except byte 0, which is
        // only written after _everything_ is confirmed to be happy.
        for addr in 1..PAGE_SIZE {
            dev.write(addr, seed)?;
        }

        Ok(())
    }

    fn validate_eeprom(dev: &At24csw080, seed: u8) -> Result<(), Error> {
        // At this point, we have already overwritten byte 0 with 0xFF
        // to avoid getting stuck in a bad validation state.

        for addr in 1..PAGE_SIZE {
            assert_eq!((addr, dev.read::<u8>(addr)?), (addr, seed));
        }

        // Test single-byte reads
        let mut i = seed;
        for addr in PAGE_SIZE..EEPROM_SIZE {
            assert_eq!((addr, dev.read::<u8>(addr)?), (addr, next(&mut i)));
        }

        // Test multi-byte reads (page-aligned)
        let mut i = seed;
        for addr in (PAGE_SIZE..EEPROM_SIZE).step_by(4) {
            let mut buf = [0u8; 4];
            buf.iter_mut().for_each(|b| *b = next(&mut i));

            assert_eq!(dev.read::<[u8; 4]>(addr)?, buf);
        }

        // Test multi-byte reads (misaligned)
        let mut i = seed;
        for addr in (PAGE_SIZE..EEPROM_SIZE - 17).step_by(17) {
            let mut buf = [0u8; 17];
            buf.iter_mut().for_each(|b| *b = next(&mut i));
            assert_eq!(dev.read::<[u8; 17]>(addr)?, buf);
        }

        // Test multi-byte reads (misaligned)
        let mut i = seed;
        for addr in (PAGE_SIZE..EEPROM_SIZE - 31).step_by(31) {
            let mut buf = [0u8; 31];
            buf.iter_mut().for_each(|b| *b = next(&mut i));
            assert_eq!(dev.read::<[u8; 31]>(addr)?, buf);
        }

        // Check the security register bytes, which should have a
        // pseudorandom pattern based on the seed.
        let mut h = seed;
        for i in 0..16 {
            let v = dev.read_security_register_byte(i)? ^ next(&mut h);
            assert_eq!(v, dev.read_security_register_byte(i + 16)?);
        }

        Ok(())
    }
}

/// Tests that task restart works as expected.
///
/// This is not a very thorough test right now.
fn test_restart() {
    let assist = assist_task_id();

    // First, store a value in state in the assistant task. More precisely, the
    // value is swapped for the previous contents, which should be zero.
    let value = 0xDEAD_F00D_u32;
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::Store as u16,
        &value.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    // Check that the old stored value (returned) is the bootup value
    assert_eq!(response, 0);

    // Read it back and replace it.
    let value2 = 0x1DE_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::Store as u16,
        &value2.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    assert_eq!(response, value);

    // Reboot the assistant and renew our task ID.
    restart_assistant();
    let assist = assist_task_id();

    // Swap values again.
    let (rc, len) = sys_send(
        assist,
        AssistOp::Store as u16,
        &value.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    // Confirm that the assistant lost our old value and returned to boot state.
    assert_eq!(response, 0);
}

/// Tests that when our task dies, we get an error code that consists of
/// the new generation in the lower bits.
fn test_restart_taskgen() {
    let assist = assist_task_id();

    // Ask the assistant to panic.
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::Panic as u16,
        &0u32.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    // Read status back from the kernel, check it, and bounce the assistant.
    let status = kipc::read_task_status(ASSIST.get_task_index().into());
    assert_eq!(
        status,
        TaskState::Faulted {
            fault: FaultInfo::Panic,
            original_state: SchedState::Runnable,
        },
    );
    restart_assistant();

    // Now when we make another call with the old task, this should fail
    // with a hint as to our generation.
    let payload = 0xDEAD_F00Du32;
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::SendBack as u16,
        &payload.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );

    assert_eq!(rc & 0xffff_ff00, 0xffff_ff00);
    assert_eq!(len, 0);
    assert_ne!(assist.generation(), Generation::from((rc & 0xff) as u8));

    assert_eq!(
        assist_task_id().generation(),
        Generation::from((rc & 0xff) as u8)
    );
}

/// Tests that the basic `borrow_info` mechanics work by soliciting a
/// stereotypical loan from the assistant.
fn test_borrow_info() {
    let assist = assist_task_id();

    // Ask the assistant to call us back with two particularly shaped loans
    // (which are hardcoded in the assistant, not encoded here).
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::SendBackWithLoans as u16,
        &0u32.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    // Don't actually care about the response in this case

    // Receive...
    hl::recv_without_notification(
        response.as_bytes_mut(),
        |_op: u32, msg| -> Result<(), u32> {
            let (_msg, caller) = msg.fixed::<u32, u32>().unwrap();

            // Borrow 0 is expected to be 16 bytes long and R/W.
            let info0 = caller.borrow(0).info().unwrap();
            assert_eq!(
                info0.attributes,
                LeaseAttributes::READ | LeaseAttributes::WRITE
            );
            assert_eq!(info0.len, 16);

            // Borrow 1 is expected to be 5 bytes long and R/O.
            let info1 = caller.borrow(1).info().unwrap();
            assert_eq!(info1.attributes, LeaseAttributes::READ);
            assert_eq!(info1.len, 5);

            caller.reply(0);
            Ok(())
        },
    );
}

/// Tests that the `sys_borrow_read` facility is working on a basic level.
fn test_borrow_read() {
    let assist = assist_task_id();

    // Ask the assistant to call us back with two particularly shaped loans
    // (which are hardcoded in the assistant, not encoded here).
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::SendBackWithLoans as u16,
        &0u32.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    // Don't actually care about the response in this case

    // Receive:
    hl::recv_without_notification(
        response.as_bytes_mut(),
        |_op: u32, msg| -> Result<(), u32> {
            let (_msg, caller) = msg.fixed::<u32, u32>().unwrap();

            // Borrow #1 is the read-only one.

            let mut dest = [0; 5];
            // Read whole buffer
            caller.borrow(1).read_fully_at(0, &mut dest).unwrap();
            assert_eq!(&dest, b"hello");

            // Read just a part
            caller.borrow(1).read_fully_at(2, &mut dest[..3]).unwrap();
            assert_eq!(&dest[..3], b"llo");

            caller.reply(0);
            Ok(())
        },
    );
}

/// Tests that the `sys_borrow_write` facility is working on a basic level.
fn test_borrow_write() {
    let assist = assist_task_id();

    // Ask the assistant to call us back with two particularly shaped loans
    // (which are hardcoded in the assistant, not encoded here).
    let mut response = 0_u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::SendBackWithLoans as u16,
        &0u32.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    // Don't actually care about the response in this case

    hl::recv_without_notification(
        response.as_bytes_mut(),
        |_op: u32, msg| -> Result<(), u32> {
            let (_msg, caller) = msg.fixed::<u32, u32>().unwrap();

            // Borrow #0 is the read-write one.

            // Complete overwrite of buffer:
            caller.borrow(0).write_at(0, *b"hello, world(s)!").unwrap();

            let mut readback = [0; 16];
            caller.borrow(0).read_fully_at(0, &mut readback).unwrap();
            assert_eq!(&readback, b"hello, world(s)!");

            // Partial overwrite:
            caller.borrow(0).write_at(7, *b"llama").unwrap();

            caller.borrow(0).read_fully_at(0, &mut readback).unwrap();
            assert_eq!(&readback, b"hello, llama(s)!");

            caller.reply(0);
            Ok(())
        },
    );
}

/// Tests the three borrow syscalls on a task that is not waiting in reply,
/// which should return `DEFECT` but not cause either task to fault.
fn test_borrow_without_peer_waiting() {
    let initial_id = assist_task_id();

    // First, try getting borrow info (which shouldn't exist)
    let info = sys_borrow_info(initial_id, 0);
    assert!(info.is_none(), "expected to fail sys_borrow_info");
    let new_id = sys_refresh_task_id(initial_id);
    assert_eq!(initial_id, new_id, "id should not change");

    // Next, attempt to do a non-existent borrow read
    let mut buf = [0; 16];
    let (rc, _n) = sys_borrow_read(initial_id, 0, 0, &mut buf);
    assert_eq!(rc, DEFECT, "expected to fail sys_borrow_read");
    let new_id = sys_refresh_task_id(initial_id);
    assert_eq!(initial_id, new_id, "id should not change");

    // Finally, attempt to do a non-existent borrow read
    let (rc, _n) = sys_borrow_write(initial_id, 0, 0, &mut buf);
    assert_eq!(rc, DEFECT, "expected to fail sys_borrow_write");
    let new_id = sys_refresh_task_id(initial_id);
    assert_eq!(initial_id, new_id, "id should not change");
}

/// Tests that faults in tasks are reported to the supervisor.
///
/// NOTE: this test depends on the supervisor fault mask, set in the test's
/// app.toml file, being `1`.
fn test_supervisor_fault_notification() {
    // First, clear the supervisor's stored notifications.
    read_runner_notifications();
    // Make sure they really cleared. Paranoia.
    assert_eq!(read_runner_notifications(), 0);

    // Now, ask the assistant to panic.
    {
        let assist = assist_task_id();
        let mut response = 0_u32;
        // Request a crash
        let (rc, len) = sys_send(
            assist,
            AssistOp::Panic as u16,
            &0u32.to_le_bytes(),
            response.as_bytes_mut(),
            &[],
        );
        assert_eq!(rc, 0);
        assert_eq!(len, 4);
        // Don't actually care about the response in this case
    }

    // Now, check the status.
    let n = read_runner_notifications();
    // The expected bitmask here is set in app.toml.
    assert_eq!(n, 1);
}

/// Tests that we can see the kernel timer advancing.
///
/// This test will fail by hanging. We can't set an iteration limit because who
/// knows how fast our computer is in relation to the tick rate?
fn test_timer_advance() {
    let initial_time = sys_get_timer().now;
    while sys_get_timer().now == initial_time {
        // doot doot
    }
}

/// Tests that we can set a timer in the future and receive a notification.
fn test_timer_notify() {
    const ARBITRARY_NOTIFICATION: u32 = 1 << 16;

    let start_time = sys_get_timer().now;
    // We'll arbitrarily set our deadline 2 ticks in the future.
    let deadline = start_time + 2;
    sys_set_timer(Some(deadline), ARBITRARY_NOTIFICATION);

    let rm = sys_recv_closed(&mut [], ARBITRARY_NOTIFICATION, TaskId::KERNEL)
        .unwrap();

    assert_eq!(rm.sender, TaskId::KERNEL);
    assert_eq!(rm.operation, ARBITRARY_NOTIFICATION);
    assert_eq!(rm.message_len, 0);
    assert_eq!(rm.response_capacity, 0);
    assert_eq!(rm.lease_count, 0);

    // In the interest of not making this test performance-sensitive, we merely
    // verify that the timer is at _or beyond_ our deadline.
    assert!(sys_get_timer().now >= deadline);
}

/// Tests that we can set a timer in the past and get immediate notification.
fn test_timer_notify_past() {
    const ARBITRARY_NOTIFICATION: u32 = 1 << 16;

    let start_time = sys_get_timer().now;
    let deadline = start_time;
    sys_set_timer(Some(deadline), ARBITRARY_NOTIFICATION);

    let rm = sys_recv_closed(&mut [], ARBITRARY_NOTIFICATION, TaskId::KERNEL)
        .unwrap();

    assert_eq!(rm.sender, TaskId::KERNEL);
    assert_eq!(rm.operation, ARBITRARY_NOTIFICATION);
    assert_eq!(rm.message_len, 0);
    assert_eq!(rm.response_capacity, 0);
    assert_eq!(rm.lease_count, 0);
}

/// Tests that floating point registers are properly saved and restored
#[cfg(any(armv7m, armv8m))]
fn test_floating_point(highregs: bool) {
    unsafe fn read_regs(dest: &mut [u32; 16], highregs: bool) {
        if !highregs {
            asm!("vstm {0}, {{s0-s15}}", in(reg) dest);
        } else {
            asm!("vstm {0}, {{s16-s31}}", in(reg) dest);
        }
    }

    let mut before = [0u32; 16];
    let mut after = [0u32; 16];

    unsafe {
        read_regs(&mut before, highregs);
    }

    // This makes the assumption that floating point has not been used in the
    // suite before the execution of this test.  Note that if floating point
    // registers are not being saved and restored properly, it is conceivable
    // that this test will fail on this assert on runs that aren't the first
    // run after reset.
    for i in 0..16 {
        assert_eq!(before[i], 0);
    }

    // Now let's make a call to our assistant to splat its floating point regs
    let assist = assist_task_id();

    let mut response = 0_u32;
    let which: u32 = if highregs { 1 } else { 0 };

    let (rc, len) = sys_send(
        assist,
        AssistOp::EatSomePi as u16,
        &which.to_le_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    unsafe {
        read_regs(&mut after, highregs);
    }

    // And verify that our registers are what we think that they should be
    for i in 0..16 {
        assert_eq!(before[i], after[i]);
    }
}

#[cfg(any(armv7m, armv8m))]
fn test_floating_point_lowregs() {
    test_floating_point(false);
}

#[cfg(any(armv7m, armv8m))]
fn test_floating_point_highregs() {
    test_floating_point(true);
}

#[cfg(any(armv7m, armv8m))]
fn test_floating_point_fault() {
    test_fault(AssistOp::PiAndDie, 0);
}

fn test_task_config() {
    // The TASK_CONFIG struct is constructed by the `task_config!` macro in
    // cooperation with the `app.toml` file.  These values are hard-coded
    // in `app.toml`, so this tests that they were correctly injected.
    assert_eq!(TASK_CONFIG.foo, "Hello, world");
    assert_eq!(TASK_CONFIG.bar, 42);
    assert_eq!(TASK_CONFIG.baz, [1, 2, 3, 4]);
    assert_eq!(TASK_CONFIG.tup, [(1, true), (2, true), (3, false)]);
}

fn test_task_status() {
    let mut id: usize = 0;
    let assist = assist_task_id();

    loop {
        let mut response = 0_u32;
        let (rc, len) = sys_send(
            assist,
            AssistOp::ReadTaskStatus as u16,
            &id.to_le_bytes(),
            response.as_bytes_mut(),
            &[],
        );
        assert_eq!(rc, 0);
        assert_eq!(len, 4);

        let status = kipc::read_task_status(ASSIST.get_task_index().into());

        if let TaskState::Faulted { fault, .. } = status {
            assert_eq!(id, NUM_TASKS);
            assert_eq!(
                fault,
                FaultInfo::SyscallUsage(UsageError::TaskOutOfRange)
            );
            return;
        }

        assert_ne!(id, NUM_TASKS);
        id += 1;
    }
}

fn test_task_fault_injection() {
    // Assistant should be fine
    let status = kipc::read_task_status(ASSIST.get_task_index().into());
    match status {
        TaskState::Healthy(..) => {}
        _ => {
            panic!("assistant is not healthy");
        }
    }

    // Inject a fault into it
    kipc::fault_task(ASSIST.get_task_index().into());

    // Assistant should now be faulted, indicating us as the injector
    let status = kipc::read_task_status(ASSIST.get_task_index().into());

    if let TaskState::Faulted { fault, .. } = status {
        if let FaultInfo::Injected(injector) = fault {
            assert_eq!(injector.index(), SUITE.get_task_index().into());
        } else {
            panic!("unexpected fault: {:?}", fault);
        }
    } else {
        panic!("unexpected status: {:?}", status);
    }
}

/// Tests that we can get current task IDs for the assistant. In practice, this
/// is already tested because the test runner relies on it -- but this may
/// provide a more specific failure if we break it, and is meant to complement
/// the bogus cases below.
fn test_refresh_task_id_basic() {
    let initial_id = assist_task_id();
    restart_assistant();
    let new_id = sys_refresh_task_id(initial_id);

    assert_eq!(
        new_id.index(),
        initial_id.index(),
        "should not change the task index"
    );
    assert_eq!(
        new_id.generation(),
        initial_id.generation().next(),
        "generation should be advanced by one here"
    );
}

fn test_refresh_task_id_off_by_one() {
    let fault = test_fault(AssistOp::RefreshTaskIdOffByOne, 0);

    assert_eq!(fault, FaultInfo::SyscallUsage(UsageError::TaskOutOfRange));
}

fn test_refresh_task_id_off_by_many() {
    let fault = test_fault(AssistOp::RefreshTaskIdOffByMany, 0);

    assert_eq!(fault, FaultInfo::SyscallUsage(UsageError::TaskOutOfRange));
}

/// Tests that notification bit posting works roughly as we'd expect.
fn test_post() {
    let assist = assist_task_id();

    let mut response = 0_u32;

    // Do an initial call to drain any previously posted bits.
    let unused = 0u32;
    let (rc, len) = sys_send(
        assist,
        AssistOp::ReadNotifications as u16,
        unused.as_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    // Now, post some bits.
    const ARBITRARY_MASK: u32 = 0xAA00006A;
    let post_rc = sys_post(assist, ARBITRARY_MASK);
    // Should not have died.
    assert_eq!(post_rc, 0);

    // And read them back.
    let (rc, len) = sys_send(
        assist,
        AssistOp::ReadNotifications as u16,
        unused.as_bytes(),
        response.as_bytes_mut(),
        &[],
    );
    assert_eq!(rc, 0);
    assert_eq!(len, 4);

    assert_eq!(response, ARBITRARY_MASK);
}

///////////////////////////////////////////////////////////////////////////////
// Frameworky bits follow

// Identity of our "assistant task" that we require in the image.
task_slot!(ASSIST, assist);
// Identity of the Idol server that we require in the image
task_slot!(IDOL, idol);

// Our own identity
task_slot!(SUITE, suite);
task_slot!(RUNNER, runner);

/// Gets the current expected `TaskId` for the assistant.
fn assist_task_id() -> TaskId {
    ASSIST.get_task_id()
}

fn idol_handle() -> test_idol_api::IdolTest {
    test_idol_api::IdolTest::from(IDOL.get_task_id())
}

/// Restarts the assistant task.
fn restart_assistant() {
    kipc::restart_task(ASSIST.get_task_index().into(), true);
}

/// Contacts the runner task to read (and clear) its accumulated set of
/// notifications.
fn read_runner_notifications() -> u32 {
    let runner = RUNNER.get_task_id();
    let mut response = 0u32;
    let op = RunnerOp::ReadAndClearNotes as u16;
    let (rc, len) = sys_send(runner, op, &[], response.as_bytes_mut(), &[]);
    assert_eq!(rc, 0);
    assert_eq!(len, 4);
    response
}

/// Actual entry point.
#[export_name = "main"]
fn main() -> ! {
    // Work out the assistant generation. Restart it to ensure it's running
    // before we try talking to it. TODO: this is kind of gross, we need a way
    // to just ask.
    kipc::restart_task(ASSIST.get_task_index().into(), true);
    loop {
        let assist = assist_task_id();
        let challenge = 0xDEADBEEF_u32;
        let mut response = 0_u32;
        let (rc, _) = sys_send(
            assist,
            0,
            &challenge.to_le_bytes(),
            response.as_bytes_mut(),
            &[],
        );
        if rc == 0 {
            break;
        }
    }

    let mut buffer = [0; 4];
    loop {
        hl::recv_without_notification(
            &mut buffer,
            |op, msg| -> Result<(), u32> {
                match op {
                    SuiteOp::GetCaseCount => {
                        let (_, caller) =
                            msg.fixed::<(), usize>().ok_or(2u32)?;
                        caller.reply(TESTS.len());
                    }
                    SuiteOp::GetCaseName => {
                        let (&idx, caller) =
                            msg.fixed::<usize, [u8; 64]>().ok_or(2u32)?;
                        let mut name_buf = [b' '; 64];
                        let name = TESTS[idx].0;
                        let name_len = name.len().min(64);
                        name_buf[..name_len]
                            .copy_from_slice(&name.as_bytes()[..name_len]);
                        caller.reply(name_buf);
                    }
                    SuiteOp::RunCase => {
                        let (&idx, caller) =
                            msg.fixed::<usize, ()>().ok_or(2u32)?;
                        let caller_tid = caller.task_id();
                        caller.reply(());

                        TESTS[idx].1();

                        let op = RunnerOp::TestComplete as u16;

                        // Call back with status.
                        let (rc, len) =
                            sys_send(caller_tid, op, &[], &mut [], &[]);
                        assert_eq!(rc, 0);
                        assert_eq!(len, 0);
                    }
                }
                Ok(())
            },
        )
    }
}

include!(concat!(env!("OUT_DIR"), "/consts.rs"));
