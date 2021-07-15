//! Client API for Jefe

#![no_std]

use zerocopy::{AsBytes, FromBytes};

use userlib::*;

#[derive(Copy, Clone, Debug, FromPrimitive, PartialEq)]
#[repr(u32)]
pub enum JefeError {
    /// Server has died
    Dead = core::u32::MAX,
    /// Invalid operation
    BadOperation = 1,
    /// Bad response
    BadResponse = 2,
    /// Bad argument
    BadArg = 3,
    /// Bad disposition value
    BadDisposition = 4,
    /// Bad task value
    BadTask = 5,
    /// Illegal task value
    IllegalTask = 6,
}

impl From<JefeError> for u32 {
    fn from(rc: JefeError) -> Self {
        rc as u32
    }
}

impl From<u32> for JefeError {
    fn from(code: u32) -> Self {
        match JefeError::from_u32(code) {
            Some(err) => err,
            None => JefeError::BadResponse
        }
    }
}

#[derive(FromPrimitive, Copy, Clone, Debug, PartialEq)]
pub enum Disposition {
    Restart = 1,
    Start = 2,
    Hold = 3,
    Fault = 4,
}

/// The actual requests that we honor from an external source entity
#[derive(FromPrimitive, PartialEq)]
pub enum Op {
    SetDisposition = 1,
}

#[derive(AsBytes, FromBytes)]
#[repr(C)]
pub struct SetDispositionRequest {
    pub task: u16,
    pub disposition: u8,
    pad: u8,
}

impl hl::Call for SetDispositionRequest {
    const OP: u16 = Op::SetDisposition as u16;
    type Response = ();
    type Err = JefeError;
}

#[derive(Clone, Debug)]
pub struct Jefe(pub TaskId);

impl From<TaskId> for Jefe {
    fn from(t: TaskId) -> Self {
        Self(t)
    }
}

impl Jefe {
    pub fn set_disposition(
        &self,
        task: TaskId,
        disposition: Disposition
    ) -> Result<(), JefeError> {
        hl::send(
            self.0,
            &SetDispositionRequest {
                task: task.index() as u16,
                disposition: disposition as u8,
                pad: 0u8,
            }
        )
    }
}
