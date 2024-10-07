#!/bin/bash
#:
#: name = "sign sp-1"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: rust_toolchain = true
#: output_rules = [
#:  "=/work/*.zip",
#: ]
#: access_repos = [
#:	"oxidecomputer/permission-slip",
#: ]
#: [dependencies.gimlet-c]
#: job = "build gimlet-c"
#:
#: [dependencies.gimlet-c-lab]
#: job = "build gimlet-c-lab"
#:
#: [dependencies.gimlet-d]
#: job = "build gimlet-d"
#:
#: [dependencies.gimlet-d-lab]
#: job = "build gimlet-d-lab"
#:
#: [dependencies.gimlet-e]
#: job = "build gimlet-e"
#:
#: [dependencies.gimlet-e-lab]
#: job = "build gimlet-e-lab"
#:
#: [dependencies.gimlet-f]
#: job = "build gimlet-f"
#:
#: [dependencies.gimlet-f-lab]
#: job = "build gimlet-f-lab"
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
