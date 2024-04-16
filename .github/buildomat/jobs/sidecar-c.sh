#!/bin/bash
#:
#: name = "build-sidecar-c"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/sidecar/rev-c.toml sidecar-c default 
