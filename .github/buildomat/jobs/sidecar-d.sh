#!/bin/bash
#:
#: name = "build-sidecar-d"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/sidecar/rev-d.toml sidecar-d default 
