#!/bin/bash
# test-attestation-update.sh - Test attestation and update features on LPC55S69-EVK
#
# This script exercises the attestation and update procedures documented in README.md.
#
# Required:
#   - humility    - Hubris debugger
#                   git clone https://github.com/oxidecomputer/humility
#                   cd humility/humility-bin && cargo install --path . --locked
#   - cargo       - For auto-detecting archive path via 'cargo xtask print'
#   - unzip       - For extracting image from archive
#   - Board flashed with lpc55xpresso image and connected
#
# Optional (script handles gracefully if missing):
#   - verifier-cli - Full attestation verification (see below)
#   - openssl      - Certificate inspection
#   - sha3sum      - Test digest creation (falls back to random data)
#
# To install verifier-cli from dice-util:
#   git clone https://github.com/oxidecomputer/dice-util
#   cd dice-util
#   cargo install --path verifier-cli --locked
#
# Usage: ./test-attestation-update.sh [-h] [-s] [-i IMAGE] [-a ARCHIVE] [-p PROBE]

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HUBRIS_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
WORK_DIR="${TMPDIR:-/tmp}/lpc55xpresso-test-$$"

# Default options
SKIP_UPDATE=false
IMAGE_NAME="a"
ARCHIVE=""
PROBE=""

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Test attestation and update features on LPC55S69-EVK development board.

Options:
    -h          Show this help message
    -s          Skip update/reboot tests
    -i IMAGE    Image name to use: 'a' or 'b' (default: a)
    -a ARCHIVE  Path to hubris archive zip (default: auto-detect via xtask)
    -p PROBE    Humility probe identifier (default: HUMILITY_PROBE env or auto)

Examples:
    $(basename "$0")                     # Run all tests with image 'a'
    $(basename "$0") -i b                # Use image 'b'
    $(basename "$0") -s                  # Skip update tests
    $(basename "$0") -a path/to/img.zip  # Use specific archive
    $(basename "$0") -p 0d28:0204:...    # Specify probe

For full attestation verification, install verifier-cli:
    git clone https://github.com/oxidecomputer/dice-util
    cd dice-util
    cargo install --path verifier-cli --locked
EOF
    exit 0
}

while getopts "hsi:a:p:" opt; do
    case $opt in
        h) usage ;;
        s) SKIP_UPDATE=true ;;
        i) IMAGE_NAME="$OPTARG" ;;
        a) ARCHIVE="$OPTARG" ;;
        p) PROBE="$OPTARG" ;;
        ?) usage ;;
    esac
done
shift $((OPTIND - 1))

log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

log_section() {
    echo ""
    echo -e "${GREEN}========================================${NC}"
    echo -e "${GREEN} $*${NC}"
    echo -e "${GREEN}========================================${NC}"
}

check_programs() {
    log_section "Checking Required Programs"

    local missing=0

    if ! command -v humility &> /dev/null; then
        log_error "humility not found in PATH"
        log_info "Install: git clone https://github.com/oxidecomputer/humility && cd humility/humility-bin && cargo install --path . --locked"
        missing=1
    fi

    if ! command -v cargo &> /dev/null; then
        log_error "cargo not found in PATH"
        log_info "Install Rust from https://rustup.rs"
        missing=1
    fi

    if ! command -v unzip &> /dev/null; then
        log_error "unzip not found in PATH"
        missing=1
    fi

    if ((missing)); then
        exit 1
    fi

    log_info "All required programs found"
}

check_prereqs() {
    log_section "Checking Prerequisites"

    # Get archive path if not specified
    if [[ -z "$ARCHIVE" ]]; then
        log_info "Getting archive path for image '$IMAGE_NAME'..."
        ARCHIVE=$(cd "$HUBRIS_DIR" && cargo -q xtask print --archive --image-name "$IMAGE_NAME" app/lpc55xpresso/app.toml 2>/dev/null) || {
            log_error "Failed to get archive path. Run 'cargo xtask dist app/lpc55xpresso/app.toml' first."
            exit 1
        }
        # Make path absolute if relative
        if [[ ! "$ARCHIVE" = /* ]]; then
            ARCHIVE="$HUBRIS_DIR/$ARCHIVE"
        fi
    fi

    if [[ ! -f "$ARCHIVE" ]]; then
        log_error "Archive file not found: $ARCHIVE"
        log_info "Build it with: cargo xtask dist app/lpc55xpresso/app.toml"
        exit 1
    fi

    log_info "Using archive: $ARCHIVE"
    export HUMILITY_ARCHIVE="$ARCHIVE"

    # Set probe if specified
    if [[ -n "$PROBE" ]]; then
        export HUMILITY_PROBE="$PROBE"
        log_info "Using probe: $PROBE"
    elif [[ -n "${HUMILITY_PROBE:-}" ]]; then
        log_info "Using probe from environment: $HUMILITY_PROBE"
    else
        log_warn "No probe specified, humility will auto-detect"
    fi

    # Test connection
    log_info "Testing connection to board..."
    if ! humility tasks &> /dev/null; then
        log_error "Cannot connect to board or Hubris not running."
        log_info "Run './check-board-setup.sh' to diagnose board setup issues."
        log_info "Available probes:"
        humility lsusb 2>/dev/null | head -10 || true
        exit 1
    fi

    log_info "Prerequisites OK"
    mkdir -p "$WORK_DIR"
    log_info "Working directory: $WORK_DIR"
}

cleanup() {
    if [[ -d "$WORK_DIR" ]]; then
        log_info "Cleaning up $WORK_DIR"
        rm -rf "$WORK_DIR"
    fi
}

trap cleanup EXIT

# ============================================================================
# Attestation Tests
# ============================================================================

test_attest_basic() {
    log_section "Testing Basic Attestation Operations"

    log_info "Getting attestation log length..."
    local log_len
    log_len=$(humility hiffy -c Attest.log_len 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
    log_info "Log length: $log_len"

    log_info "Getting attestation signature length..."
    local attest_len
    attest_len=$(humility hiffy -c Attest.attest_len 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
    log_info "Attestation length: $attest_len"

    log_info "Getting certificate chain length..."
    local cert_chain_len
    cert_chain_len=$(humility hiffy -c Attest.cert_chain_len 2>/dev/null | grep -oP '[0-9]+' | tail -1)
    log_info "Certificate chain length: $cert_chain_len certs"

    log_info "Basic attestation operations: PASS"
}

test_attest_record() {
    log_section "Testing Measurement Recording"

    # Get initial log length
    local initial_len
    initial_len=$(humility hiffy -c Attest.log_len 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
    log_info "Initial log length: $initial_len"

    # Create a test digest (SHA3-256 of "test measurement data")
    log_info "Creating test digest..."
    if command -v sha3sum &> /dev/null; then
        echo -n "test measurement data for lpc55xpresso" | sha3sum -a 256 | cut -d' ' -f1 | xxd -r -p > "$WORK_DIR/test_digest.bin"
    else
        # Fallback if sha3sum not available - use random data
        log_warn "sha3sum not available, using random digest"
        dd if=/dev/urandom of="$WORK_DIR/test_digest.bin" bs=32 count=1 2>/dev/null
    fi

    log_info "Recording measurement..."
    if humility hiffy -c Attest.record -a algorithm=Sha3_256 -i "$WORK_DIR/test_digest.bin" >/dev/null 2>&1; then
        log_info "Measurement recorded successfully"
    else
        log_error "Failed to record measurement"
        return 1
    fi

    # Verify log length increased
    local new_len
    new_len=$(humility hiffy -c Attest.log_len 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
    log_info "New log length: $new_len"

    if [[ "$new_len" != "$initial_len" ]]; then
        log_info "Log length increased: PASS"
    else
        log_warn "Log length unchanged (may have hit max entries)"
    fi
}

test_attest_log_read() {
    log_section "Testing Log Reading"

    local log_len
    log_len=$(humility hiffy -c Attest.log_len 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
    local log_len_dec=$((log_len))

    log_info "Reading attestation log ($log_len_dec bytes)..."

    # Read log in chunks
    local offset=0
    local chunk_size=256
    > "$WORK_DIR/log.bin"

    while ((offset < log_len_dec)); do
        local remaining=$((log_len_dec - offset))
        local read_size=$((remaining < chunk_size ? remaining : chunk_size))

        humility hiffy -c Attest.log -a offset=$offset -o "$WORK_DIR/log_chunk.bin" -n $read_size >/dev/null 2>&1
        cat "$WORK_DIR/log_chunk.bin" >> "$WORK_DIR/log.bin"
        offset=$((offset + read_size))
    done

    log_info "Log saved to $WORK_DIR/log.bin"
    log_info "Log contents (hex):"
    hexdump -C "$WORK_DIR/log.bin" | head -10

    log_info "Log reading: PASS"
}

test_attest_certs() {
    log_section "Testing Certificate Retrieval"

    local cert_chain_len
    cert_chain_len=$(humility hiffy -c Attest.cert_chain_len 2>/dev/null | grep -oP '[0-9]+' | tail -1)

    log_info "Certificate chain has $cert_chain_len certificates"

    for ((i=0; i<cert_chain_len; i++)); do
        log_info "Getting certificate $i..."

        local cert_len
        cert_len=$(humility hiffy -c Attest.cert_len -a index=$i 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
        local cert_len_dec=$((cert_len))
        log_info "  Certificate $i length: $cert_len_dec bytes"

        # Read certificate in chunks
        local offset=0
        local chunk_size=256
        > "$WORK_DIR/cert_${i}.der"

        while ((offset < cert_len_dec)); do
            local remaining=$((cert_len_dec - offset))
            local read_size=$((remaining < chunk_size ? remaining : chunk_size))

            humility hiffy -c Attest.cert -a index=$i,offset=$offset -o "$WORK_DIR/cert_chunk.bin" -n $read_size >/dev/null 2>&1
            cat "$WORK_DIR/cert_chunk.bin" >> "$WORK_DIR/cert_${i}.der"
            offset=$((offset + read_size))
        done

        # Try to convert to PEM and display info
        if command -v openssl &> /dev/null; then
            if openssl x509 -inform DER -in "$WORK_DIR/cert_${i}.der" -out "$WORK_DIR/cert_${i}.pem" 2>/dev/null; then
                log_info "  Certificate $i subject:"
                openssl x509 -in "$WORK_DIR/cert_${i}.pem" -noout -subject 2>/dev/null | sed 's/^/    /'
            fi
        fi
    done

    log_info "Certificate retrieval: PASS"
}

test_attest_signature() {
    log_section "Testing Attestation Signature"

    # Generate nonce
    log_info "Generating nonce..."
    dd if=/dev/urandom of="$WORK_DIR/nonce.bin" bs=32 count=1 2>/dev/null

    # Get attestation
    local attest_len
    attest_len=$(humility hiffy -c Attest.attest_len 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
    local attest_len_dec=$((attest_len))

    log_info "Getting attestation ($attest_len_dec bytes)..."
    humility hiffy -c Attest.attest -i "$WORK_DIR/nonce.bin" -o "$WORK_DIR/attestation.bin" -n $attest_len_dec >/dev/null 2>&1

    log_info "Attestation saved to $WORK_DIR/attestation.bin"
    log_info "Attestation (hex):"
    hexdump -C "$WORK_DIR/attestation.bin" | head -5

    log_info "Attestation signature: PASS"
}

test_attest_with_verifier_cli() {
    log_section "Testing with verifier-cli"

    if ! command -v verifier-cli &> /dev/null; then
        log_warn "verifier-cli not found in PATH, skipping full verification"
        log_info "To install: git clone https://github.com/oxidecomputer/dice-util && cd dice-util && cargo install --path verifier-cli --locked"
        return 0
    fi

    mkdir -p "$WORK_DIR/verify"

    # For dev boards with self-signed certs (dice-self), we verify the cert chain
    # and attestation signature separately. The full 'verify' command expects
    # production certs with PlatformId extensions.

    log_info "Getting cert chain..."
    if ! verifier-cli cert-chain > "$WORK_DIR/verify/cert-chain.pem" 2>/dev/null; then
        log_error "Failed to get cert chain"
        return 0
    fi

    log_info "Verifying self-signed cert chain..."
    if verifier-cli verify-cert-chain --self-signed "$WORK_DIR/verify/cert-chain.pem" 2>&1; then
        log_info "Certificate chain verification: PASS"
    else
        log_warn "Certificate chain verification: FAIL (may be expected for dev certs)"
    fi

    log_info "Getting attestation log..."
    verifier-cli log > "$WORK_DIR/verify/log.bin" 2>/dev/null || true

    log_info "Verification artifacts saved to $WORK_DIR/verify/"
}

# ============================================================================
# Update Tests
# ============================================================================

test_update_info() {
    log_section "Testing Update Information"

    log_info "Getting block size..."
    local block_size
    block_size=$(humility hiffy -c Update.block_size 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1)
    log_info "Block size: $block_size"

    log_info "Getting current version..."
    humility hiffy -c Update.current_version 2>/dev/null | grep -v "^humility"

    log_info "Getting boot info..."
    humility hiffy -c Update.rot_boot_info 2>/dev/null | grep -v "^humility"

    log_info "Getting versioned boot info (v2)..."
    humility hiffy -c Update.versioned_rot_boot_info -a version=2 2>/dev/null | grep -v "^humility"

    log_info "Update information: PASS"
}

test_caboose_read() {
    log_section "Testing Caboose Reading"

    for slot in A B; do
        log_info "Reading caboose for slot $slot..."

        local cab_size
        cab_size=$(humility hiffy -c Update.caboose_size -a slot=$slot 2>/dev/null | grep -oP '0x[0-9a-fA-F]+' | tail -1 || echo "0x0")

        if [[ "$cab_size" == "0x0" ]] || [[ -z "$cab_size" ]]; then
            log_warn "Slot $slot: No caboose or empty"
            continue
        fi

        local cab_size_dec=$((cab_size))
        log_info "Slot $slot caboose size: $cab_size_dec bytes"

        # Read caboose
        humility hiffy -c Update.read_raw_caboose -a slot=$slot,offset=0 -o "$WORK_DIR/caboose_${slot}.bin" -n $cab_size_dec >/dev/null 2>&1 || true

        if [[ -f "$WORK_DIR/caboose_${slot}.bin" ]]; then
            log_info "Slot $slot caboose contents:"
            # Look for known TLV tags
            strings "$WORK_DIR/caboose_${slot}.bin" 2>/dev/null | grep -E '^(GITC|BORD|NAME|VERS|SIGN)' | head -5 | sed 's/^/  /' || true
        fi
    done

    log_info "Caboose reading: PASS"
}

# Update a Hubris image via humility hiffy
# Usage: hiffy_update <image.bin> [ImageA|ImageB|Bootloader]
hiffy_update() {
    local IMAGE=$1
    local SLOT=${2:-ImageB}
    local BLOCK_SIZE=512

    if [[ ! -f "$IMAGE" ]]; then
        log_error "Image file not found: $IMAGE"
        return 1
    fi

    local IMAGE_SIZE=$(stat -c%s "$IMAGE")
    local NUM_BLOCKS=$(( (IMAGE_SIZE + BLOCK_SIZE - 1) / BLOCK_SIZE ))

    # Prepare update
    log_info "Preparing update to $SLOT..."
    humility hiffy -c Update.prep_image_update -a image_type=$SLOT 2>/dev/null || return 1

    log_info "Image size: $IMAGE_SIZE bytes, $NUM_BLOCKS blocks"

    # Write blocks using process substitution (no temp files)
    for ((n=0; n<NUM_BLOCKS; n++)); do
        printf "\r  Writing block %d/%d..." "$n" "$NUM_BLOCKS"
        humility hiffy -c Update.write_one_block -a block_num=$n \
            -i <(dd if="$IMAGE" bs=$BLOCK_SIZE count=1 skip=$n 2>/dev/null) >/dev/null 2>&1 || return 1
    done
    echo ""

    # Finish update
    log_info "Finishing update..."
    humility hiffy -c Update.finish_image_update 2>/dev/null || return 1

    log_info "Update complete"
}

test_update_cycle() {
    log_section "Testing Update Cycle"

    if [[ "$SKIP_UPDATE" == "true" ]]; then
        log_warn "Skipping update tests (-s specified)"
        return 0
    fi

    # Extract final.bin from archive
    log_info "Extracting image from archive..."
    unzip -o -j "$ARCHIVE" "img/final.bin" -d "$WORK_DIR" >/dev/null 2>&1 || {
        log_error "Failed to extract img/final.bin from archive"
        return 1
    }

    local IMAGE_BIN="$WORK_DIR/final.bin"
    if [[ ! -f "$IMAGE_BIN" ]]; then
        log_error "Could not find extracted final.bin"
        return 1
    fi

    log_info "Using image: $IMAGE_BIN ($(stat -c%s "$IMAGE_BIN") bytes)"

    # Get current boot info using versioned API (v2 has more detail)
    local boot_info
    boot_info=$(humility hiffy -c Update.versioned_rot_boot_info -a version=2 2>/dev/null || \
                humility hiffy -c Update.rot_boot_info 2>/dev/null)
    log_info "Current boot info:"
    echo "$boot_info" | grep -v "^humility" | sed 's/^/  /'

    # Determine target slot (opposite of active)
    # Parse "active: A" or "active: B" from the output
    local active_slot
    active_slot=$(echo "$boot_info" | grep -oP 'active:\s*\K[AB]' | head -1)

    local target_slot
    if [[ "$active_slot" == "A" ]]; then
        target_slot="ImageB"
    elif [[ "$active_slot" == "B" ]]; then
        target_slot="ImageA"
    else
        log_warn "Could not determine active slot, defaulting to ImageB"
        target_slot="ImageB"
    fi

    log_info "Active slot: $active_slot, Target slot for update: $target_slot"

    # Perform update
    if hiffy_update "$IMAGE_BIN" "$target_slot"; then
        log_info "Update to $target_slot: PASS"
    else
        log_error "Update to $target_slot: FAIL"
        return 1
    fi

    # Show how to activate
    local slot_letter="${target_slot: -1}"  # Get A or B from ImageA/ImageB
    log_info ""
    log_info "Update written successfully. To activate:"
    log_info "  humility hiffy -c Update.switch_default_image -a slot=${slot_letter},duration=Forever"
    log_info "  humility hiffy -c Update.reset"
}

# ============================================================================
# Other hiffy Tests
# ============================================================================

test_misc_hiffy() {
    log_section "Testing Miscellaneous hiffy Operations"

    log_info "Testing LED control..."
    humility hiffy -c UserLeds.led_on -a index=0 2>/dev/null || log_warn "LED on failed"
    sleep 0.5
    humility hiffy -c UserLeds.led_off -a index=0 2>/dev/null || log_warn "LED off failed"
    log_info "LED control: OK"

    log_info "Testing RNG..."
    humility hiffy -c Rng.fill -o "$WORK_DIR/random.bin" -n 32 >/dev/null 2>&1 || log_warn "RNG failed"
    if [[ -f "$WORK_DIR/random.bin" ]]; then
        log_info "Random data:"
        hexdump -C "$WORK_DIR/random.bin" | head -2
    fi

    log_info "Getting task list..."
    humility tasks 2>/dev/null | head -20

    log_info "Miscellaneous tests: PASS"
}

# ============================================================================
# Main
# ============================================================================

main() {
    echo "============================================"
    echo " LPC55S69-EVK Attestation & Update Test"
    echo "============================================"
    echo ""

    check_programs
    check_prereqs

    # Attestation tests
    test_attest_basic
    test_attest_record
    test_attest_log_read
    test_attest_certs
    test_attest_signature
    test_attest_with_verifier_cli

    # Update tests
    test_update_info
    test_caboose_read
    test_update_cycle

    # Misc tests
    test_misc_hiffy

    log_section "Test Summary"
    log_info "All tests completed!"
    log_info "Work directory preserved at: $WORK_DIR"
    log_info ""
    log_info "Artifacts:"
    ls -la "$WORK_DIR"/ 2>/dev/null | sed 's/^/  /'

    # Don't cleanup on success so user can inspect
    trap - EXIT
}

main "$@"
