#!/bin/bash

set -o errexit
set -o pipefail
set -o xtrace

toml=$1
app_name=$2
images=$3

if [ ! -f $toml ]; then
	echo "$toml does not exist"
	exit 1
fi

if [ -z ${app_name} ]; then
	echo "Missing app name"
	exit 1
fi

if [ -z $images ]; then
	echo "Missing images"
	exit 1
fi

banner build
mkdir /work/hubris

# We intentionally omit rust-toolchain above and install rustup/cargo
# here with no default toolchain since rustup will pickup whatever
# toolchain (probably nightly!) we have in our repo
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | /bin/bash -s - \
        -y --no-modify-path --default-toolchain none --profile default

cargo xtask dist $toml
for image in $images; do
	cp target/${app_name}/dist/$image/build-${app_name}-image-$image.zip \
		/work/hubris/build-${app_name}-image-$image.zip
done
