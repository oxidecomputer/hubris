#!/bin/bash
#:
#: name = "build-gimlet-c"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/gimlet/rev-c.toml gimlet-c default 
