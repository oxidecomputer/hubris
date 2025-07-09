// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

#![no_std]
#![forbid(clippy::wildcard_imports)]

use userlib::FromPrimitive;

/// Operations that are performed by the test-assist
#[derive(FromPrimitive, Debug, Eq, PartialEq)]
pub enum AssistOp {
    JustReply = 0,
    SendBack = 1,
    LastReply = 2,
    BadMemory = 3,
    Panic = 4,
    Store = 5,
    SendBackWithLoans = 6,
    #[cfg(any(armv7m, armv8m))]
    DivZero = 7,
    StackOverflow = 8,
    ExecData = 9,
    IllegalOperation = 10,
    BadExec = 11,
    TextOutOfBounds = 12,
    StackOutOfBounds = 13,
    BusError = 14,
    IllegalInstruction = 15,
    #[cfg(any(armv7m, armv8m))]
    EatSomePi = 16,
    #[cfg(any(armv7m, armv8m))]
    PiAndDie = 17,
    ReadTaskStatus = 18,
    FaultTask = 19,
    RestartTask = 20,
    RefreshTaskIdOffByOne = 21,
    RefreshTaskIdOffByMany = 22,
    ReadNotifications = 23,
    FastPanic = 24, // panic before sending a reply
}

/// Operations that are performed by the test-suite
#[derive(FromPrimitive)]
pub enum SuiteOp {
    /// Run a case, replying before it starts (`usize -> ()`).
    RunCase = 3,
}

/// Operations that are performed by the test-runner
#[derive(FromPrimitive)]
pub enum RunnerOp {
    /// Reads out, and clears, the accumulated set of notifications we've
    /// received (`() -> u32`).
    ReadAndClearNotes = 0,
    /// Indicates that the test suite would like the test runner to trigger an
    /// IRQ.
    SoftIrq = 1,
    /// Enables automatic restarts for non-test-suite tasks that crash
    AutoRestart = 2,
    /// Signals that a test is complete, and that the runner is switching back
    /// to passive mode (`() -> ()`).
    TestComplete = 0xfffe,
    /// Returns the result of the last test if it completed
    TestResult = 0xffff,
}

#[derive(FromPrimitive)]
#[repr(u32)]
pub enum TestResult {
    Failure = 0,
    Success = 1,
    NotDone = 3,
}

impl TryFrom<u32> for TestResult {
    type Error = u32;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(TestResult::Failure),
            1 => Ok(TestResult::Success),
            3 => Ok(TestResult::NotDone),
            x => Err(x),
        }
    }
}
