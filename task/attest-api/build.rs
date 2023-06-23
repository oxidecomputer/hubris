// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use idol::client;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    client::build_client_stub("../../idl/attest.idol", "client_stub.rs")?;
    Ok(())
}
