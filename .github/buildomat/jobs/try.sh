#!/bin/bash
#:
#: name = "try"
#: variety = "basic"
#: target = "hubris"
#:

set -o errexit
set -o pipefail
set -o xtrace

humility --version

humility lsusb

export HUMILITY_PROBE='1fc9:0143:CFSDRHMTTXVSJ'

humility probe


