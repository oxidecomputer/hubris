#!/bin/bash
#:
#: name = "build-gimlet-f"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/gimlet/rev-f.toml gimlet-f default 
