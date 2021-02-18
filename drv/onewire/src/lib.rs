#![no_std]

use userlib::*;

#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub enum Command {
    SearchROM = 0xf0,
    ReadROM = 0x33,
    MatchROM = 0x55,
    SkipROM = 0xcc,
    AlarmSearch = 0xec,
}

#[derive(Copy, Clone, PartialEq, FromPrimitive)]
pub enum Family {
    DS18B20 = 0x28,
}

pub fn family(id: u64) -> Option<Family> {
    Family::from_u8((id & 0xff) as u8)
}

pub fn search<T>(
    reset_search: impl Fn() -> Result<(), T>,
    triplet: impl Fn(bool) -> Result<(bool, bool), T>,
    branches: (u64, u64),
) -> Result<(u64, (u64, u64)), T> {
    let mut rval = 0;
    let mut rbranches = (0u64, 0u64);

    reset_search()?;

    for i in 0..64 {
        //
        // If this is our deepest branch, flip its disposition to be that it's
        // taken.
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
