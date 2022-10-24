// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use abi::ImageHeader;
use core::mem::MaybeUninit;

// This is updated by build scripts (which is why this is marked as no_mangle)
// Although we don't access any fields of the header from hubris right now, it
// is safer to treat this as MaybeUninit in case we need to do so in the future.
#[used]
#[no_mangle]
#[link_section = ".image_header"]
static HEADER: MaybeUninit<ImageHeader> = MaybeUninit::uninit();
