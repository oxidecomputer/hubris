#!/bin/bash
#:
#: name = "build-psc-b"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/psc/rev-b.toml psc-b default 
