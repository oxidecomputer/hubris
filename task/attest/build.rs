// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use idol::server::{self, ServerStyle};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    server::build_server_support(
        "../../idl/attest.idol",
        "server_stub.rs",
        ServerStyle::InOrder,
    )?;
    Ok(())
}
