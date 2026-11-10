#!/bin/bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
#:
#: name = "field-of-rakes"
#: variety = "basic"
#: target = "ubuntu-22.04-large"
#: rust_toolchain = true
#: enable = true


set -o errexit
set -o pipefail
set -o xtrace


_cleanup () {
	kill $(jobs -p)
}


trap _cleanup SIGINT SIGTERM EXIT

rustup toolchain install
cargo xtask dist app/cosmo/rev-b.toml


pushd /work

# Maybe pin or build?
curl -LO https://github.com/oxidecomputer/humility/releases/download/nightly/humility
chmod +x humility

git clone https://github.com/oxidecomputer/sp-emu.git
cd sp-emu
cargo build --release

popd

HUMILITY_ARCHIVE=target/cosmo-b/dist/default/build-cosmo-b-image-default.zip

/work/sp-emu/target/release/sp-emu flash a ${HUMILITY_ARCHIVE}
/work/sp-emu/target/release/sp-emu run a 0 &

sleep 20

/work/humility -a ${HUMILITY_ARCHIVE} -p 20b7:9db1:tcp:127.0.0.1:4444 tasks
/work/humility -a ${HUMILITY_ARCHIVE} -p 20b7:9db1:tcp:127.0.0.1:4444 dump

