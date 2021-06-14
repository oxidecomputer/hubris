#![no_std]

use userlib::*;

/// Operations that are performed by the test-assist
#[derive(FromPrimitive, Debug, PartialEq)]
pub enum AssistOp {
    JustReply = 0,
    SendBack = 1,
    LastReply = 2,
    BadMemory = 3,
    Panic = 4,
    Store = 5,
    SendBackWithLoans = 6,
    DivZero = 7,
    StackOverflow = 8,
    ExecData = 9,
    IllegalOperation = 10,
    BadExec = 11,
    TextOutOfBounds = 12,
    StackOutOfBounds = 13,
    BusError = 14,
    IllegalInstruction = 15,
    EatSomePi = 16,
    PiAndDie = 17,
    ReadTaskStatus = 18,
    FaultTask = 19,
    RestartTask = 20,
}

/// Operations that are performed by the test-suite
#[derive(FromPrimitive)]
pub enum SuiteOp {
    /// Get the number of test cases (`() -> usize`).
    GetCaseCount = 1,
    /// Get the name of a case (`usize -> [u8]`).
    GetCaseName = 2,
    /// Run a case, replying before it starts (`usize -> ()`).
    RunCase = 3,
}

/// Operations that are performed by the test-runner
#[derive(FromPrimitive)]
pub enum RunnerOp {
    /// Reads out, and clears, the accumulated set of notifications we've
    /// received (`() -> u32`).
    ReadAndClearNotes = 0,
    /// Signals that a test is complete, and that the runner is switching back
    /// to passive mode (`() -> ()`).
    TestComplete = 0xFFFF,
}
