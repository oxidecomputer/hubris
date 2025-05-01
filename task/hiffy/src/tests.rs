// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use byteorder::ByteOrder;
use hif::*;
use ringbuf::*;
use test_api::*;
#[allow(unused_imports)]
use userlib::{sys_send, task_slot};
use zerocopy::IntoBytes;

task_slot!(TEST_TASK, suite);
task_slot!(RUNNER, runner);

// arg0: test id number
pub(crate) fn run_a_test(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.is_empty() {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 1;

    let id = match stack[fp + 0] {
        Some(id) => id,
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(0)));
        }
    };

    userlib::kipc::restart_task(TEST_TASK.get_task_index().into(), true);

    ringbuf_entry!(Trace::RunTest(id));
    let (rc, _len) = sys_send(
        TEST_TASK.get_task_id(),
        SuiteOp::RunCase as u16,
        id.as_bytes(),
        &mut [],
        &[],
    );

    if rc != 0 {
        return Err(Failure::FunctionError(rc));
    }

    let mut result: u32 = TestResult::NotDone as u32;

    loop {
        let (rc, _len) = sys_send(
            RUNNER.get_task_id(),
            RunnerOp::TestResult as u16,
            &[],
            result.as_mut_bytes(),
            &[],
        );

        if rc != 0 {
            return Err(Failure::FunctionError(rc));
        }

        match TestResult::try_from(result) {
            Ok(x) => match x {
                TestResult::Success => {
                    byteorder::LittleEndian::write_u32(rval, 1);
                    return Ok(core::mem::size_of::<u32>());
                }
                TestResult::Failure => {
                    byteorder::LittleEndian::write_u32(rval, 0);
                    return Ok(core::mem::size_of::<u32>());
                }
                TestResult::NotDone => (),
            },
            Err(x) => return Err(Failure::FunctionError(x)),
        }
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Execute((usize, hif::Op)),
    Failure(Failure),
    Success,
    RunTest(u32),
    None,
}

ringbuf!(Trace, 64, Trace::None);

pub enum Functions {
    RunTest(usize, bool),
}

pub(crate) static HIFFY_FUNCS: &[Function] = &[run_a_test];

//
// This definition forces the compiler to emit the DWARF needed for debuggers
// to be able to know function indices, arguments and return values.
//
#[no_mangle]
#[used]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) fn trace_execute(offset: usize, op: hif::Op) {
    ringbuf_entry!(Trace::Execute((offset, op)));
}

pub(crate) fn trace_success() {
    ringbuf_entry!(Trace::Success);
}

pub(crate) fn trace_failure(f: hif::Failure) {
    ringbuf_entry!(Trace::Failure(f));
}
