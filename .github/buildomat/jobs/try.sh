#!/bin/bash
#:
#: name = "try"
#: variety = "basic"
#: target = "hubris-test"
#:

set -o errexit
set -o pipefail
set -o xtrace

humility --version

humility lsusb

export HUMILITY_PROBE='1fc9:0143:CFSDRHMTTXVSJ'

humility probe

#
# Here is one we prepared earlier:
#
curl -sSf -o sp.zip 'https://pkg.oxide.computer/tmp/build-gimletlet-image-default.zip'

export HUMILITY_ARCHIVE=sp.zip

humility flash
humility tasks

