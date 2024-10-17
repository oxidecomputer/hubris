#!/bin/bash
#:
#: name = "build gimlet-c"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: rust_toolchain = true
#: output_rules = [
#:  "=/work/*.zip",
#: ]

set -o errexit
set -o pipefail
set -o xtrace

exec .github/buildomat/build-one.sh gimlet-e /app/gimlet/rev-e.toml default
