#!/bin/bash

set -o errexit
set -o pipefail
set -o xtrace
set -o nounset

toml=$1
name=$2
image=$3

cargo xtask dist $toml
cp target/$name/dist/$image/build-$name-image-$image.zip /work/
