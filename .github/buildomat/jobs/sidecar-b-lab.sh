#!/bin/bash
#:
#: name = "build sidecar-b-lab"
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

exec .github/buildomat/build-one.sh sidecar-b-lab app/sidecar/rev-b-lab.toml default
