# LPC55Xpresso (LPC55S69-EVK) Development Board

This app.toml targets the NXP LPC55S69-EVK development board for testing
Hubris features without production hardware.

## LPC55 memory layout

The LPC55 has a first stage bootloader before booting hubris. This bootloader
runs in secure mode before transitioning to non-secure mode. The code currently
makes the assumption that hubris starts right at the end of the stage0
bootloader. This needs to be set appropriately in app.toml! The minimum
alignment for flash is 0x8000.

```
+----------------+  0x98000
|                |
|                |
|                |
|                |
|                |
|                |
|   Hubris       |
|                |
|                |
|                |
|                |
|                |
|                |
+----------------+  0x8000
|                |
|   stage0       |
|                |
+----------------+  0x0
```

## Board Setup (Factory-Fresh Boards)

If you have a factory-fresh LPC55S69-EVK board, it needs to be initialized
before running the tests. Use the setup check script to verify your board:

```bash
./check-board-setup.sh
```

This script:
- Checks required programs are installed (humility, cargo, unzip, tlvctool)
- Scans for connected debug probes
- Identifies LPC55S69-EVK boards by reading the caboose BORD field
- Verifies CMPA/CFPA is configured
- Checks Stage0 bootloader is installed
- Verifies Hubris image is present
- Provides ready-to-use `export HUMILITY_PROBE=...` commands

You can also specify a probe explicitly:

```bash
./check-board-setup.sh -p <probe-id>
```

If the board is not set up, follow these steps:

```bash
# 1. Initialize CMPA/CFPA and flash bootloader
humility rebootleby app/lpc55xpresso/bootleby-lpc55xpresso.zip --yes-really

# 2. Build the Hubris image
cargo xtask dist app/lpc55xpresso/app.toml

# 3. Flash the Hubris image
humility flash

# 4. Verify setup
./check-board-setup.sh
```

### Installing Required Tools

```bash
# humility - Hubris debugger
git clone https://github.com/oxidecomputer/humility
cd humility/humility-bin && cargo install --path . --locked

# tlvctool - TLV parsing for caboose inspection
git clone https://github.com/oxidecomputer/tlvc
cd tlvc && cargo install --path tlvctool --locked

# verifier-cli - Attestation verification (optional but recommended)
git clone https://github.com/oxidecomputer/dice-util
cd dice-util && cargo install --path verifier-cli --locked
```

## Attestation Testing

This board is configured to allow testing of the attestation feature via
`humility hiffy` commands. The `hiffy` task has been granted permission to
record measurements (via `permit_log_reset`), which is normally reserved
for the `swd` task that measures the SP on production RoT hardware.

Since this dev board doesn't have an SP to measure, you can:
1. Record test measurements manually via hiffy
2. Read the attestation log
3. Get signed attestations
4. Verify the certificate chain (self-signed via the `dice-self` kernel feature)

### Prerequisites

Set up humility environment variables to point to the board. The
`check-board-setup.sh` script will identify LPC55S69-EVK boards and
provide the correct probe ID:

```bash
# Run check script to get probe ID
./check-board-setup.sh

# Set environment variables (use probe ID from check script output)
export HUMILITY_PROBE=<probe-id-from-check-script>

# Archive is auto-detected by test scripts, but can be set explicitly:
export HUMILITY_ARCHIVE=target/lpc55xpresso/dist/a/build-lpc55xpresso-image-a.zip
```

### Basic Attestation Operations with humility hiffy

```bash
# Check attestation log length (starts empty or with boot measurements)
humility hiffy -c Attest.log_len

# Get attestation signature length
humility hiffy -c Attest.attest_len

# Record a test measurement (32-byte SHA3-256 digest)
# First create a test digest:
echo -n "test data to measure" | sha3sum -a 256 | xxd -r -p > /tmp/test_digest.bin
humility hiffy -c Attest.record -a algorithm=Sha3_256 -i /tmp/test_digest.bin

# Read the attestation log (chunked due to buffer limits)
humility hiffy -c Attest.log -a offset=0 -o /tmp/log_part1.bin -n 256

# Get certificate chain length
humility hiffy -c Attest.cert_chain_len

# Get first certificate (alias cert used for signing attestations)
humility hiffy -c Attest.cert_len -a index=0
humility hiffy -c Attest.cert -a index=0,offset=0 -o /tmp/alias_cert.der -n <cert_len>

# Get a signed attestation with a nonce
dd if=/dev/urandom of=/tmp/nonce.bin bs=32 count=1
humility hiffy -c Attest.attest -i /tmp/nonce.bin -o /tmp/attestation.bin -n 65
```

### Using verifier-cli (from dice-util)

The `verifier-cli` tool in the `dice-util` repository provides a higher-level
interface for attestation operations. It wraps `humility hiffy` and handles
chunked reads, serialization, and verification.

```bash
# Clone dice-util if you don't have it
# git clone https://github.com/oxidecomputer/dice-util

cd /path/to/dice-util

# Generate a nonce
dd if=/dev/urandom of=nonce.bin bs=32 count=1

# Get an attestation (signature over log || nonce)
cargo run --bin verifier-cli -- attest nonce.bin > attestation.bin

# Get the certificate chain (PEM format)
cargo run --bin verifier-cli -- cert-chain > cert-chain.pem

# Get the measurement log
cargo run --bin verifier-cli -- log > log.bin

# Get the alias certificate (used for signing)
cargo run --bin verifier-cli -- cert 0 > alias.pem

# Record a measurement (hashes the input file with SHA3-256)
cargo run --bin verifier-cli -- record /path/to/file/to/measure

# Verify the attestation signature
cargo run --bin verifier-cli -- verify-attestation \
    --alias-cert alias.pem \
    --log log.json \
    --nonce nonce.json \
    attestation.json

# Verify the certificate chain (self-signed for dice-self builds)
cargo run --bin verifier-cli -- verify-cert-chain --self-signed cert-chain.pem

# Full verification workflow (generates nonce, fetches everything, verifies)
cargo run --bin verifier-cli -- verify --self-signed
```

### Verifying with OpenSSL

You can also verify the certificate chain and attestation signatures using
standard OpenSSL commands. The DICE certificates use Ed25519 keys.

```bash
# Extract alias certificate in PEM format
humility hiffy -c Attest.cert -a index=0,offset=0 -o /tmp/alias.der -n <cert_len>
openssl x509 -inform DER -in /tmp/alias.der -out /tmp/alias.pem

# View certificate details
openssl x509 -in /tmp/alias.pem -text -noout

# For self-signed chains, verify the root cert signs itself
openssl verify -CAfile /tmp/root.pem /tmp/root.pem

# Verify intermediate is signed by root
openssl verify -CAfile /tmp/root.pem /tmp/intermediate.pem

# Verify alias is signed by intermediate
openssl verify -CAfile /tmp/intermediate.pem /tmp/alias.pem
```

Note: The attestation itself is a signature over `sha3_256(log || nonce)`.
To verify it manually, you would need to:
1. Deserialize the hubpack-encoded log
2. Concatenate log bytes with the 32-byte nonce
3. Compute SHA3-256 of the concatenation
4. Verify the Ed25519 signature using the alias certificate's public key

The `verifier-cli` handles this complexity for you.

### Example Test Session

```bash
# Build and flash the image
cargo xtask dist app/lpc55xpresso/app.toml
humility flash

# Check initial state
humility hiffy -c Attest.log_len    # Should show some initial size

# Record a test measurement
echo "my test firmware v1.0" | sha3sum -a 256 | cut -d' ' -f1 | xxd -r -p > /tmp/digest.bin
humility hiffy -c Attest.record -a algorithm=Sha3_256 -i /tmp/digest.bin

# Verify log grew
humility hiffy -c Attest.log_len    # Should be larger now

# Use verifier-cli for full verification
cd /path/to/dice-util
cargo run --bin verifier-cli -- verify --self-signed --work-dir /tmp/attest-test

# Examine the artifacts
cat /tmp/attest-test/log.json
cat /tmp/attest-test/attest.json
cat /tmp/attest-test/cert-chain.pem
```

### Attestation Data Structures

The attestation system uses these key data types (from `attest-data` crate):

- **Log**: Contains measurement entries, each with algorithm type and digest
- **Nonce**: 32-byte random value to ensure freshness
- **Attestation**: Ed25519 signature over `sha3_256(log || nonce)`
- **Certificate Chain**: DICE-derived PKI path from alias key to device identity

The measurement log slot 0 is reserved for the first privileged measurement.
On production hardware, the `swd` task records the SP firmware hash in this
slot. On this dev board, `hiffy` can record any measurement for testing.

## Update and Bootloader Testing

The LPC55S69 dev board supports testing the Hubris update mechanism via
`humility hiffy`. This allows exercising the A/B image switching, bootloader
updates, and related functionality without production hardware.

### Available Update Operations

Use `humility hiffy -l` to see all available interfaces. Key Update operations:

```bash
# Get the block size for updates (typically 512 bytes)
humility hiffy -c Update.block_size

# Get current boot info
humility hiffy -c Update.rot_boot_info
humility hiffy -c Update.versioned_rot_boot_info -a version=2

# Get current image version
humility hiffy -c Update.current_version

# Read caboose data (contains GITC, BORD, NAME, VERS, SIGN)
humility hiffy -c Update.caboose_size -a slot=A
humility hiffy -c Update.read_raw_caboose -a slot=A,offset=0 -o /tmp/caboose.bin -n <size>

# Switch boot preference (transient or persistent)
humility hiffy -c Update.switch_default_image -a slot=B,duration=Forever
humility hiffy -c Update.switch_default_image -a slot=A,duration=Once

# Reset the device
humility hiffy -c Update.reset
```

### Updating a Hubris Image via hiffy

To update a Hubris image (slot A or B), you need to send the image in blocks.
This is a multi-step process:

```bash
# 1. Build a new image to update to
cargo xtask dist app/lpc55xpresso/app.toml

# 2. Prepare the update (specify ImageA, ImageB, or Bootloader)
humility hiffy -c Update.prep_image_update -a image_type=ImageB

# 3. Get the block size
humility hiffy -c Update.block_size
# Returns: Update.block_size() => 0x200 (512)

# 4. Write blocks sequentially (block_num starts at 0)
# Each block must be exactly block_size bytes (pad with 0xFF if needed)
humility hiffy -c Update.write_one_block -a block_num=0 -i /tmp/block0.bin
humility hiffy -c Update.write_one_block -a block_num=1 -i /tmp/block1.bin
# ... continue for all blocks ...

# 5. Finish the update
humility hiffy -c Update.finish_image_update

# 6. Switch to the new image and reset
humility hiffy -c Update.switch_default_image -a slot=B,duration=Forever
humility hiffy -c Update.reset
```

### Scripted Update Helper

Splitting an image into blocks is tedious, here's a helper
function using process substitution to avoid temporary files:

```bash
# Update a Hubris image via humility hiffy
# Usage: hiffy_update <image.bin> [ImageA|ImageB|Bootloader]
hiffy_update() {
    local IMAGE=$1
    local SLOT=${2:-ImageB}
    local BLOCK_SIZE=512

    if [[ ! -f "$IMAGE" ]]; then
        echo "Usage: hiffy_update <image.bin> [ImageA|ImageB|Bootloader]"
        return 1
    fi

    # Prepare update
    echo "Preparing update to $SLOT..."
    humility hiffy -c Update.prep_image_update -a image_type=$SLOT || return 1

    # Calculate number of blocks
    local IMAGE_SIZE=$(stat -c%s "$IMAGE")
    local NUM_BLOCKS=$(( (IMAGE_SIZE + BLOCK_SIZE - 1) / BLOCK_SIZE ))
    echo "Image size: $IMAGE_SIZE bytes, $NUM_BLOCKS blocks"

    # Write blocks using process substitution (no temp files)
    for ((n=0; n<NUM_BLOCKS; n++)); do
        echo -ne "\rWriting block $n/$NUM_BLOCKS..."
        humility hiffy -c Update.write_one_block -a block_num=$n \
            -i <(dd if="$IMAGE" bs=$BLOCK_SIZE count=1 skip=$n 2>/dev/null) >/dev/null || return 1
    done
    echo ""

    # Finish update
    echo "Finishing update..."
    humility hiffy -c Update.finish_image_update || return 1

    echo "Update complete. Use Update.switch_default_image and Update.reset to boot new image."
}
```

Note: The final block may be smaller than `BLOCK_SIZE`. The LPC55 update
server handles padding internally, so partial blocks work correctly.

### Checking Boot Status

```bash
# Get detailed boot information
humility hiffy -c Update.versioned_rot_boot_info -a version=2

# This returns information including:
# - active: which slot is currently running (A or B)
# - persistent_boot_preference: default boot slot
# - pending_persistent_boot_preference: pending preference change
# - transient_boot_preference: one-time boot override
# - slot_a_fwid / slot_b_fwid: SHA3-256 hashes of each slot
# - stage0_fwid / stage0next_fwid: bootloader hashes
# - *_status: validity status of each slot
```

### Component-Level Operations

For more granular control, use the component-specific APIs:

```bash
# Read caboose for specific component and slot
humility hiffy -c Update.component_caboose_size -a component=Hubris,slot=A
humility hiffy -c Update.component_read_raw_caboose -a component=Hubris,slot=A,offset=0 -o /tmp/cab.bin -n 256

# Prepare component update
humility hiffy -c Update.component_prep_image_update -a component=Hubris,slot=B

# Switch component boot image
humility hiffy -c Update.component_switch_default_image -a component=Hubris,slot=B,duration=Forever
```

### Viewing Caboose Contents

The caboose contains build metadata. To decode it:

```bash
# Get caboose size
SIZE=$(humility hiffy -c Update.caboose_size -a slot=A 2>/dev/null | grep -oP '0x[0-9a-f]+')

# Read raw caboose
humility hiffy -c Update.read_raw_caboose -a slot=A,offset=0 -o /tmp/caboose.bin -n $((SIZE))

# View the raw content (TLV format with magic CAOB/BORD/GITC/NAME/VERS/SIGN)
hexdump -C /tmp/caboose.bin

# Or use humility's built-in caboose reading (if supported)
humility caboose
```

### Automated Test Script

A comprehensive test script is provided that exercises all attestation and
update features:

```bash
# Show help
./test-attestation-update.sh -h

# Run all tests (auto-detects archive from 'cargo xtask print')
HUMILITY_PROBE=<probe-id> ./test-attestation-update.sh

# Use image 'b' instead of default 'a'
HUMILITY_PROBE=<probe-id> ./test-attestation-update.sh -i b

# Skip update/reboot tests
HUMILITY_PROBE=<probe-id> ./test-attestation-update.sh -s

# Specify probe explicitly via -p option
./test-attestation-update.sh -p <probe-id>

# Use a specific archive file
./test-attestation-update.sh -a /path/to/archive.zip
```

The test script:
- Auto-detects the archive path via `cargo xtask print`
- Uses `versioned_rot_boot_info` (v2) for detailed boot status
- Automatically targets the non-active slot for updates (avoids `RunningImage` errors)
- Uses `verifier-cli` for certificate chain verification if available

The script tests:
- Basic attestation operations (log length, cert chain length)
- Measurement recording
- Log reading (chunked)
- Certificate retrieval and display
- Attestation signature generation
- Certificate chain verification (via verifier-cli)
- Update information queries (versioned boot info, block size)
- Caboose reading
- Update cycle (writes image to non-active slot)
- LED and RNG operations

### Other Useful hiffy Operations

```bash
# List all available interfaces
humility hiffy -l

# Control LEDs (useful for visual feedback during testing)
humility hiffy -c UserLeds.led_on -a index=0
humility hiffy -c UserLeds.led_off -a index=0
humility hiffy -c UserLeds.led_toggle -a index=1

# Read GPIO pins
humility hiffy -c Pins.read_val -a 'pin={port=0,pin=5}'

# Get random data from RNG
humility hiffy -c Rng.fill -o /tmp/random.bin -n 32

# Trigger a chip reset via syscon
humility hiffy -c Syscon.chip_reset

# Dump task state
humility hiffy -c Jefe.dump_task -a task_index=5
```

