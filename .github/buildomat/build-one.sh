#!/bin/bash

set -o errexit
set -o pipefail
set -o xtrace
set -o nounset

name=$1
toml=$2
image=$3

rustup toolchain install
cargo xtask dist $toml
cp target/$name/dist/$image/build-$name-image-$image.zip /work/
touch /work/this_is_not_signed.txt
