#!/bin/bash
#:
#: name = "build-sidecar-b"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/sidecar/rev-b.toml sidecar-b default 
