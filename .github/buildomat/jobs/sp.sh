#!/bin/bash
#:
#: name = "build sp"
#: variety = "basic"
#: target = "ubuntu-22.04"
#: rust_toolchain = true
#: output_rules = [
#:  "=/work/*.zip",
#: ]
#: access_repos = [
#:	"oxidecomputer/permission-slip",
#:	"oxidecomputer/dogbuild",
#: ]
#:

set -o errexit
set -o pipefail
set -o xtrace

source .github/buildomat/force-git-over-https.sh
source .github/buildomat/permslip-setup.sh


while read -r line; do
	name=`echo $line | cut -d ' ' -f 1`
	toml=`echo $line | cut -d ' ' -f 2`
	cargo xtask dist $toml
	$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 sign "UNTRUSTED SP" \
			target/$name/dist/default/build-$name-image-default.zip \
			--version 0.0.0-ci > /work/$name.zip
done < .github/buildomat/sp_build_list
