#!/bin/bash
#:
#: name = "build-oxide-rot-1"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: output_rules = [
#:      "=/work/hubris/*.zip",
#: ]

exec .github/buildomat/build-one.sh app/oxide-rot-1/app.toml oxide-rot-1 "a b"
