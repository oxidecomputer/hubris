// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use call_rustfmt::rustfmt;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustfmt("/tmp/wrong.rs")?;
    Ok(())
}
