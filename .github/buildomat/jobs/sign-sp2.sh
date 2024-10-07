#!/bin/bash
#:
#: name = "sign sp-2"
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
#: [dependencies.sidecar-b]
#: job = "build sidecar-b"
#:
#: [dependencies.sidecar-b-lab]
#: job = "build sidecar-b-lab"
#
#: [dependencies.sidecar-c]
#: job = "build sidecar-c"
#:
#: [dependencies.sidecar-c-lab]
#: job = "build sidecar-c-lab"
#:
#: [dependencies.sidecar-d]
#: job = "build sidecar-d"
#:
#: [dependencies.sidecar-d-lab]
#: job = "build sidecar-d-lab"
#:
#:
#: [dependencies.psc-b]
#: job = "build psc-b"
#:
#: [dependencies.psc-c]
#: job = "build psc-c"
#

# Due to buildomat limits we can only have 8 dependent jobs so we
# split the SP signing into two different jobs


set -o errexit
set -o pipefail
set -o xtrace

_cleanup () {
        kill $(jobs -p)
}

trap _cleanup SIGINT SIGTERM EXIT

source .github/buildomat/force-git-over-https.sh
source .github/buildomat/permslip-setup.sh


for f in `find /input -type f -name "*.zip"`; do
	$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 sign "UNTRUSTED SP" \
			$f \
			--version 0.0.0-ci > /work/$(basename $f)
done
