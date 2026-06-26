#!/bin/bash
#:
#: name = "build rot"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: rust_toolchain = true
#: output_rules = [
#:  "=/work/*.zip",
#: ]
#: access_repos = [
#:	"oxidecomputer/permission-slip",
#: ]
#:

set -o errexit
set -o pipefail
set -o xtrace

_cleanup () {
	kill $(jobs -p)
}

trap _cleanup SIGINT SIGTERM EXIT

source .github/buildomat/force-git-over-https.sh
source .github/buildomat/permslip-setup.sh

rustup toolchain install

cargo xtask dist app/oxide-rot-1/app-dev.toml
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 \
	sign "UNTRUSTED bart" \
	target/oxide-rot-1-selfsigned/dist/a/build-oxide-rot-1-selfsigned-image-a.zip \
	--version 0.0.0-ci > /work/build-oxide-rot-1-selfsigned-image-a.zip

$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 \
	sign "UNTRUSTED bart" \
	target/oxide-rot-1-selfsigned/dist/b/build-oxide-rot-1-selfsigned-image-b.zip \
	--version 0.0.0-ci > /work/build-oxide-rot-1-selfsigned-image-b.zip
