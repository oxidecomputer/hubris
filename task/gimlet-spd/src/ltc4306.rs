// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//
// Virtual LTC4306 implementation.  The part is pretty simple, but this
// virtualiation is even simpler:  we do not support enabling any combination
// of segments, and don't support any of its lockup detection.
//

//
// We stick with the LTC4306 nomenclature, which has segments starting at 1,
// and names the registers with their number.
//
const SEGMENT_1: u8 = 0x80;
const SEGMENT_2: u8 = 0x40;
const SEGMENT_3: u8 = 0x20;
const SEGMENT_4: u8 = 0x10;
const REGISTER_0: u8 = 0;
const REGISTER_3: u8 = 3;

//
// We're fine, everything's fine here. How are you?
//
const CONNECTED_NOT_FAILED: u8 = 0b1000_0100;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum State {
    /// idle
    Idle,
    /// received REGISTER_3; presumed write
    AwaitingSegment,
    /// received REGISTER_0; presumed read
    TransmitStatus,
    /// something has gone wrong
    Error,
    /// operation is complete
    Done,
}

impl State {
    pub fn init() -> Self {
        State::Idle
    }

    pub fn rx(&self, byte: u8, mut segment: impl FnMut(Option<u8>)) -> Self {
        match self {
            State::AwaitingSegment => {
                match byte {
                    SEGMENT_1 => segment(Some(0)),
                    SEGMENT_2 => segment(Some(1)),
                    SEGMENT_3 => segment(Some(2)),
                    SEGMENT_4 => segment(Some(3)),
                    _ => segment(None),
                }
                State::Done
            }
            State::Idle => match byte {
                REGISTER_0 => State::TransmitStatus,
                REGISTER_3 => State::AwaitingSegment,
                _ => State::Error,
            },
            _ => State::Error,
        }
    }

    pub fn tx(&self) -> (Option<u8>, Self) {
        match self {
            State::TransmitStatus => (Some(CONNECTED_NOT_FAILED), State::Done),
            _ => (None, State::Error),
        }
    }
}
