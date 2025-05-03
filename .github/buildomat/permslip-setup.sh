#!/bin/bash

set -ex

sudo apt-get update
sudo apt-get install -y postgresql gcc pkgconf openssl libssl-dev

sudo -u postgres createuser yourname
sudo -u postgres createdb permslip
sudo -u postgres psql << EOF
\x
alter user yourname with encrypted password 'password';
EOF

sudo -u postgres psql << EOF
\x
grant all privileges on database permslip to yourname;
EOF
sudo -u postgres psql -d permslip << EOF
grant all on schema public to yourname;
EOF

export PERMSLIP_DIR=/work/permslip
BART_KEY=$(pwd)/support/fake_certs/fake_private_key.pem

mkdir -p $PERMSLIP_DIR
git clone https://github.com/oxidecomputer/permission-slip.git -b ssh_key_fix $PERMSLIP_DIR
pushd $PERMSLIP_DIR
rustup toolchain install
cargo build --release
export POSTGRES_HOST=localhost
export POSTGRES_PORT=5432
export POSTGRES_USER=yourname
export POSTGRES_PASSWORD=password

ssh-keygen -t ecdsa -b 256 -f /tmp/id_p256 -N '' -C ''
eval "$(ssh-agent -s)"
ssh-add /tmp/id_p256
PERMSLIP_SSH_KEY=$(ssh-keygen -lf /tmp/id_p256.pub | cut -d ' ' -f 2)
export PERMSLIP_SSH_KEY

$PERMSLIP_DIR/target/release/permslip-server import-ssh-key /tmp/id_p256.pub
$PERMSLIP_DIR/target/release/permslip-server import-private-key "UNTRUSTED bart" rsa "$BART_KEY"
$PERMSLIP_DIR/target/release/permslip-server start-server &

sleep 5

$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 list-keys

# SP
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 generate-key "UNTRUSTED SP" rsa
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 generate-csr "UNTRUSTED SP" > SP.csr
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 sign "UNTRUSTED SP" --kind csr SP.csr > SP.cert
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 set-key-context "UNTRUSTED SP" --kind hubris --cert SP.cert --root SP.cert

# Bart
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 generate-csr "UNTRUSTED bart" > bart.csr
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 sign "UNTRUSTED bart" --kind csr bart.csr > bart.cert
$PERMSLIP_DIR/target/release/permslip --url=http://localhost:41340 set-key-context "UNTRUSTED bart" --kind hubris --cert bart.cert --root bart.cert

popd
