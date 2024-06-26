#!/bin/bash
#:
#: name = "build-oxide-rot-1-dev"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/oxide-rot-1/app-dev.toml oxide-rot-1-dev "a b"
