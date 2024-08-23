// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! 1-wire APIs
//!
//! This crate has functions and structures specific to the 1-wire interface,
//! a single wire interface for peripheral interconnection.  While 1-wire can
//! support many different kinds of devices, we currenly only recognize the
//! DS18B20 family.
//!

#![no_std]

use userlib::FromPrimitive;

/// 1-wire commands.  Most devices support more commands, but these commands
/// are supported by all devices.
#[allow(dead_code)]
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum Command {
    SearchROM = 0xf0,
    ReadROM = 0x33,
    MatchROM = 0x55,
    SkipROM = 0xcc,
    AlarmSearch = 0xec,
}

/// Family of 1-wire device. The most complete list seems to be found at:
/// <http://owfs.sourceforge.net/family.html>.  We want to keep this list
/// as short as possible.
#[derive(Copy, Clone, Eq, PartialEq, FromPrimitive)]
pub enum Family {
    DS18B20 = 0x28,
}

/// A type alias for a 1-wire identifer
pub type Identifier = u64;

/// Given a 1-wire 64-bit identifier, returns the family (or `None` if the
/// family is unrecognized).
pub fn family(id: Identifier) -> Option<Family> {
    Family::from_u8((id & 0xff) as u8)
}

///
/// Search a 1-wire bus for the next device.
///
/// Each 1-wire device has its own (unique) 64-bit identifier, and the way
/// these values are discovered with just a single wire is actually pretty
/// nifty:  first, the bus is reset and a command is sent indicating that a
/// new search is to begin (this is done via the `reset_search` closure).
/// Then, for a given bit position, every device on the bus sends their bit at
/// that position, followed by its inverse.  If every device agrees (that is,
/// if everyone has the same value at a given bit position), the initiator
/// will see the value followed by its inverse.  If, however, there is
/// disagreement, the initiator will see the same value twice (some devices
/// will pull the bus low for the bit, some will pull it low for its inverse).
/// At that point -- a branch point -- the initiator will send either a 0 or 1
/// to indicate which path it wants to take.  Devices on the bus will look
/// for this bit indicating desired path; those whose bit in the current
/// position does not match the bit sent will fall out, remaining
/// silent for the reset of the search.
///
/// The routine that sends communication to the 1-wire bus is the closure
/// `triplet`: it takes a boolean, which tells the initiator what its
/// disposition should be in the event of a branch at this bit position (that
/// is, if it should choose a 1 vs. a 0).  This closure returns a tuple,
/// indicating the direction taken -- and if that direction constitutes a
/// branch (that is, if some devices have a 0 and others a 1 at this bit
/// position).  This routine will be called 64 times (once for each bit
/// position), with the return value being a tuple consisting of the resulting
/// identifier (itself representing the path taken through the tree of device
/// IDs) along with the branch state 2-tuple.
///
/// On subsequent calls to search for additional devices, the returned branch
/// state 2-tuple should be passed back in `branches`; this will assure that
/// the subsequent search function makes a different decision at its deepest
/// branch.  Once both branches have been taken for a given branching point in
/// the tree, that subtree is pruned.
///
/// This algorithm is neat, but it's not quick -- and if the 1-wire device is
/// sitting on the other side of an I2C bridge, it's even slower.
///
pub fn search<T>(
    reset_search: impl Fn() -> Result<(), T>,
    triplet: impl Fn(bool) -> Result<(bool, bool), T>,
    branches: (Identifier, Identifier),
) -> Result<(Identifier, (Identifier, Identifier)), T> {
    let mut rval = 0;
    let mut rbranches = (0u64, 0u64);

    reset_search()?;

    for i in 0..64 {
        //
        // If this is our deepest branch, flip our disposition to take the
        // other branch -- otherwise do what we did last time through.
        //
        let take = if branches.0 & (1 << i) != 0 {
            if i == 63 || 1 << (i + 1) > branches.0 {
                true
            } else {
                branches.1 & (1 << i) != 0
            }
        } else {
            false
        };

        let (took, branched) = triplet(take)?;

        if branched {
            rbranches.0 |= 1 << i;

            if took {
                rbranches.1 |= 1 << i;
            }
        }

        if took {
            rval |= 1 << i;
        }
    }

    //
    // We're done!  Before we return, we need to now prune those subtrees
    // that we've exhausted entirely.
    //
    let mut branch = 1 << 63;

    while branch != 0 {
        if rbranches.0 & branch != 0 {
            if rbranches.1 & branch != 0 {
                rbranches.0 &= !branch;
                rbranches.1 &= !branch;
            } else {
                break;
            }
        }

        branch >>= 1;
    }

    Ok((rval, rbranches))
}
