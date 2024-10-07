#!/bin/bash
#:
#: name = "build sidecar-d-lab"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: rust_toolchain = true
#: output_rules = [
#:  "=/work/*.zip",
#:  "=/work/this_is_not_signed.txt",
#: ]

set -o errexit
set -o pipefail
set -o xtrace

exec .github/buildomat/build-one.sh sidecar-d-lab app/sidecar/rev-d-lab.toml default
