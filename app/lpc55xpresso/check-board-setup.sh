#!/bin/bash
# check-board-setup.sh - Check if LPC55S69-EVK is properly set up for testing
#
# This script checks that a factory-fresh LPC55S69-EVK board has been properly
# initialized with:
#   - CMPA/CFPA configuration
#   - Stage0 bootloader
#   - Hubris image
#
# If the board is not set up, it provides instructions on how to initialize it.
#
# Usage: ./check-board-setup.sh [-h] [-p PROBE]

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE=""

usage() {
    cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Check if LPC55S69-EVK board is properly set up for attestation/update testing.

Options:
    -h          Show this help message
    -p PROBE    Humility probe identifier (default: HUMILITY_PROBE env or auto)

This script checks:
    1. Required programs are installed (humility, cargo, unzip)
    2. Board is connected and responding
    3. CMPA/CFPA is configured (not factory-fresh)
    4. Stage0 bootloader is present
    5. Hubris image is present in slot A

If the board is not set up, instructions are provided for initialization.
EOF
    exit 0
}

while getopts "hp:" opt; do
    case $opt in
        h) usage ;;
        p) PROBE="$OPTARG" ;;
        ?) usage ;;
    esac
done
shift $((OPTIND - 1))

log_info() {
    echo -e "${GREEN}[OK]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[FAIL]${NC} $*"
}

log_check() {
    echo -e "Checking: $*"
}

# Extract BORD field from caboose via direct memory read
# Returns the board name or empty string on failure
# Based on hubris-probe extract_caboose function
extract_bord() {
    local probe_id="$1"

    # Read header to get magic and total length (8 bytes at offset 0x130 from image base)
    local header
    header=$(humility -p "$probe_id" readmem $ROT_HEADER_BASE_A 8 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | \
        tr -d ' \n' || true)

    if [[ -z "$header" ]]; then
        return 1
    fi

    # Parse header magic (first 4 bytes = 8 hex chars) and total length (next 4 bytes)
    local magic="${header:0:8}"
    local total_len_hex="${header:8:8}"

    if [[ "$magic" != "$HUBRIS_HEADER_MAGIC" ]]; then
        return 1
    fi

    # Convert total_len from little-endian hex bytes to integer
    # e.g., "00440100" -> 0x00014400 = 82944
    local b0="${total_len_hex:0:2}"
    local b1="${total_len_hex:2:2}"
    local b2="${total_len_hex:4:2}"
    local b3="${total_len_hex:6:2}"
    local total_len=$((0x${b3}${b2}${b1}${b0}))

    # Read caboose length (last 4 bytes of image)
    local caboose_len_bytes
    caboose_len_bytes=$(humility -p "$probe_id" readmem $((ROT_BASE_A + total_len - 4)) 4 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | \
        tr -d ' \n' || true)

    if [[ -z "$caboose_len_bytes" ]]; then
        return 1
    fi

    # Convert from little-endian
    b0="${caboose_len_bytes:0:2}"
    b1="${caboose_len_bytes:2:2}"
    b2="${caboose_len_bytes:4:2}"
    b3="${caboose_len_bytes:6:2}"
    local caboose_len=$((0x${b3}${b2}${b1}${b0}))

    # Verify caboose magic
    local caboose_magic
    caboose_magic=$(humility -p "$probe_id" readmem $((ROT_BASE_A + total_len - caboose_len)) 4 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | tr -d ' \n' || true)

    if [[ "$caboose_magic" != "$CABOOSE_MAGIC" ]]; then
        return 1
    fi

    # Read caboose data and look for BORD field in ASCII portion
    local caboose_ascii
    caboose_ascii=$(humility -p "$probe_id" readmem $((ROT_BASE_A + total_len - caboose_len + 4)) $((caboose_len - 4)) 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[3];}' | tr -d '\n' || true)

    # Look for "lpcxpresso55s69" in the ASCII portion
    if echo "$caboose_ascii" | grep -q "lpcxpresso55s69"; then
        echo "lpcxpresso55s69"
        return 0
    fi

    return 1
}

# Memory addresses for LPC55S69
CMPA_BOOT_CONFIG=0x9e400
CMPA_RKTH=0x9e450
ROT_BASE_A=0x00010000
ROT_HEADER_BASE_A=$((ROT_BASE_A + 0x130))
HUBRIS_HEADER_MAGIC="cad6ce64"  # Little-endian 0x64ced6ca
CABOOSE_MAGIC="5e00b0ca"        # Little-endian 0xcab0005e

declare -i SETUP_OK=1
SETUP_ISSUES=()
DISCOVERED_PROBES=()

# Check required programs
check_programs() {
    echo ""
    echo "=== Checking Required Programs ==="

    local missing=0

    log_check "humility"
    if command -v humility &> /dev/null; then
        log_info "humility found: $(which humility)"
    else
        log_error "humility not found"
        echo "  Install: git clone https://github.com/oxidecomputer/humility"
        echo "           cd humility/humility-bin && cargo install --path . --locked"
        missing=1
    fi

    log_check "cargo"
    if command -v cargo &> /dev/null; then
        log_info "cargo found: $(which cargo)"
    else
        log_error "cargo not found"
        echo "  Install Rust from https://rustup.rs"
        missing=1
    fi

    log_check "unzip"
    if command -v unzip &> /dev/null; then
        log_info "unzip found: $(which unzip)"
    else
        log_error "unzip not found"
        echo "  Install: sudo apt install unzip (or equivalent)"
        missing=1
    fi

    log_check "tlvctool"
    if command -v tlvctool &> /dev/null; then
        log_info "tlvctool found: $(which tlvctool)"
    else
        log_error "tlvctool not found"
        echo "  Install: git clone https://github.com/oxidecomputer/tlvc"
        echo "           cd tlvc && cargo install --path tlvctool --locked"
        missing=1
    fi

    if ((missing)); then
        SETUP_OK=0
        SETUP_ISSUES+=("Required programs missing")
        return 1
    fi
    return 0
}

# Check board connection
check_connection() {
    echo ""
    echo "=== Checking Board Connection ==="

    # Save and unset incoming HUMILITY_PROBE so we can probe each board explicitly
    local incoming_probe="${HUMILITY_PROBE:-}"
    unset HUMILITY_PROBE

    log_check "Debug probe connection"

    # Try to list probes (filter out failures and warnings to reduce noise)
    local probes_output
    probes_output=$(humility lsusb 2>&1 || true)

    if [[ -z "$probes_output" ]] || echo "$probes_output" | grep -q "no connected probes"; then
        log_error "No debug probes found"
        echo "  - Connect the LPC55S69-EVK board via USB"
        echo "  - The board has an on-board LPC-Link2 debug probe"
        SETUP_OK=0
        SETUP_ISSUES+=("No debug probe connected")
        return 1
    fi

    # Extract only the successfully opened probes (skip failures and warnings)
    local successful_probes
    successful_probes=$(echo "$probes_output" | \
        sed -n '/--- successfully opened/,/--- failures/p' | \
        grep -E '^humility: [0-9a-f]{4}:[0-9a-f]{4}:' || true)

    if [[ -z "$successful_probes" ]]; then
        # Fallback: try to get any probe lines
        successful_probes=$(echo "$probes_output" | \
            grep -E '^humility: [0-9a-f]{4}:[0-9a-f]{4}:' || true)
    fi

    echo "  Available debug probes:"
    echo "$successful_probes" | sed 's/^humility: /    /'

    # Identify LPC55S69-EVK probes by checking caboose BORD field via direct memory read
    log_check "Identifying LPC55S69-EVK boards..."

    while IFS= read -r line; do
        # Extract probe ID (VID:PID:SERIAL)
        local probe_id
        probe_id=$(echo "$line" | sed 's/^humility: //' | awk '{print $1}')
        if [[ -z "$probe_id" ]]; then
            continue
        fi

        # Try to extract BORD from caboose via direct memory read (uses -p flag)
        local bord
        bord=$(extract_bord "$probe_id" 2>/dev/null || true)

        # Check if this is an lpcxpresso55s69 board
        if [[ "$bord" == "lpcxpresso55s69" ]]; then
            DISCOVERED_PROBES+=("$probe_id")
            log_info "Found LPC55S69-EVK: $probe_id"
        fi
    done <<< "$successful_probes"

    # Determine which probe to use for remaining checks
    # Priority: -p command line option > incoming HUMILITY_PROBE > auto-select first discovered
    if [[ -n "$PROBE" ]]; then
        export HUMILITY_PROBE="$PROBE"
        log_info "Using probe from -p option: $HUMILITY_PROBE"
    elif [[ -n "$incoming_probe" ]]; then
        export HUMILITY_PROBE="$incoming_probe"
        log_info "Using probe from HUMILITY_PROBE env: $HUMILITY_PROBE"
    elif [[ ${#DISCOVERED_PROBES[@]} -eq 1 ]]; then
        export HUMILITY_PROBE="${DISCOVERED_PROBES[0]}"
        log_info "Auto-selected probe: $HUMILITY_PROBE"
    elif [[ ${#DISCOVERED_PROBES[@]} -gt 1 ]]; then
        # Multiple boards found, pick first but warn
        export HUMILITY_PROBE="${DISCOVERED_PROBES[0]}"
        log_warn "Multiple LPC55S69-EVK boards found, using first one"
        log_info "Selected probe: $HUMILITY_PROBE"
    fi

    if [[ ${#DISCOVERED_PROBES[@]} -eq 0 ]]; then
        log_warn "No LPC55S69-EVK boards found among connected probes"
        echo "  Connected probes may be other board types (Gimlet, etc.)"
    fi

    # Try to read memory to confirm connection
    log_check "Memory access"
    if [[ -n "${HUMILITY_PROBE:-}" ]] && humility readmem 0 4 >/dev/null 2>&1; then
        log_info "Board responding to memory reads"
    else
        log_error "Cannot read memory from board"
        echo "  - Check USB connection"
        echo "  - Try power cycling the board"
        if [[ ${#DISCOVERED_PROBES[@]} -gt 0 ]]; then
            echo "  - Set HUMILITY_PROBE or use -p to select a board"
        fi
        SETUP_OK=0
        SETUP_ISSUES+=("Cannot communicate with board")
        return 1
    fi

    return 0
}

# Check CMPA/CFPA configuration
check_cmpa() {
    echo ""
    echo "=== Checking CMPA/CFPA Configuration ==="

    log_check "CMPA boot configuration (address 0x9e400)"

    local boot_config
    boot_config=$(humility readmem $CMPA_BOOT_CONFIG 16 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | \
        tr -d ' \n' || echo "")

    if [[ -z "$boot_config" ]]; then
        log_error "Cannot read CMPA"
        SETUP_OK=0
        SETUP_ISSUES+=("Cannot read CMPA")
        return 1
    fi

    echo "  Boot config: $boot_config"

    if [[ "$boot_config" == "00000000000000000000000000000000" ]] || \
       [[ "$boot_config" == "ffffffffffffffffffffffffffffffff" ]]; then
        log_error "CMPA not configured (factory-fresh)"
        SETUP_OK=0
        SETUP_ISSUES+=("CMPA not configured")
        return 1
    else
        log_info "CMPA is configured"
    fi

    log_check "Root Key Table Hash (RKTH)"
    local rkth
    rkth=$(humility readmem $CMPA_RKTH 32 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | \
        tr -d ' \n' || echo "")

    echo "  RKTH: $rkth"

    if [[ "$rkth" == "0000000000000000000000000000000000000000000000000000000000000000" ]]; then
        log_warn "No root keys configured (unsigned images only)"
    else
        log_info "Root keys configured"
    fi

    return 0
}

# Check for stage0 bootloader
check_bootloader() {
    echo ""
    echo "=== Checking Stage0 Bootloader ==="

    log_check "Stage0 at address 0x0"

    # Read first few bytes of flash - should not be all FF if bootloader present
    local flash_start
    flash_start=$(humility readmem 0x0 32 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | \
        tr -d ' \n' || echo "")

    if [[ -z "$flash_start" ]]; then
        log_error "Cannot read flash"
        SETUP_OK=0
        SETUP_ISSUES+=("Cannot read flash")
        return 1
    fi

    if [[ "$flash_start" == "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff" ]]; then
        log_error "No bootloader installed (flash is erased)"
        SETUP_OK=0
        SETUP_ISSUES+=("No bootloader installed")
        return 1
    fi

    # Check for valid ARM vector table (SP and reset vector should be in valid ranges)
    # Extract hex bytes from humility readmem output (format: "0xADDR | XX XX XX XX | ...")
    local sp_bytes
    sp_bytes=$(humility readmem 0x0 4 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | \
        tr -d ' \n' || echo "ffffffff")

    # SP should point to RAM (0x20000000 range for LPC55) - first 4 bytes, little-endian
    if [[ "$sp_bytes" == "ffffffff" ]] || [[ "$sp_bytes" == "00000000" ]]; then
        log_error "Invalid vector table - bootloader may be corrupted"
        SETUP_OK=0
        SETUP_ISSUES+=("Invalid bootloader")
        return 1
    fi

    log_info "Bootloader appears to be installed"
    echo "  Initial SP bytes: $sp_bytes"

    return 0
}

# Check for Hubris image
check_hubris_image() {
    echo ""
    echo "=== Checking Hubris Image ==="

    log_check "Hubris image at slot A (address 0x10000)"

    # Read Hubris header magic
    local header
    header=$(humility readmem $ROT_HEADER_BASE_A 8 2>/dev/null | \
        awk '/^0x/ { split($0, c, "|"); print c[2];}' | \
        tr -d ' \n' || echo "")

    if [[ -z "$header" ]]; then
        log_error "Cannot read image header"
        SETUP_OK=0
        SETUP_ISSUES+=("Cannot read image header")
        return 1
    fi

    # Extract magic (first 4 bytes, little-endian)
    local magic="${header:0:8}"
    echo "  Header magic: $magic (expected: $HUBRIS_HEADER_MAGIC)"

    if [[ "$magic" == "$HUBRIS_HEADER_MAGIC" ]]; then
        log_info "Valid Hubris image found in slot A"

        # Try to get version info via hiffy
        log_check "Hubris is running and responding"
        if humility tasks >/dev/null 2>&1; then
            log_info "Hubris is running"
            echo ""
            echo "  Task list:"
            humility tasks 2>/dev/null | head -15 | sed 's/^/    /'
        else
            log_warn "Hubris image present but not responding (may need reset)"
        fi
    else
        log_error "No valid Hubris image in slot A"
        SETUP_OK=0
        SETUP_ISSUES+=("No Hubris image")
        return 1
    fi

    return 0
}

# Print setup instructions
print_setup_instructions() {
    echo ""
    echo "============================================"
    echo -e "${RED} Board Setup Required ${NC}"
    echo "============================================"
    echo ""
    echo "Issues found:"
    for issue in "${SETUP_ISSUES[@]}"; do
        echo "  - $issue"
    done
    echo ""
    echo "To initialize a factory-fresh LPC55S69-EVK board:"
    echo ""
    echo "1. Initialize CMPA/CFPA and flash bootloader:"
    echo "   cd $(dirname "$SCRIPT_DIR")"
    echo "   humility rebootleby app/lpc55xpresso/bootleby-lpc55xpresso.zip --yes-really"
    echo ""
    echo "2. Build the Hubris image:"
    echo "   cargo xtask dist app/lpc55xpresso/app.toml"
    echo ""
    echo "3. Flash the Hubris image:"
    echo "   humility flash"
    echo ""
    echo "4. Re-run this check script:"
    echo "   ./check-board-setup.sh"
    echo ""
}

# Print success message
print_success() {
    echo ""
    echo "============================================"
    echo -e "${GREEN} Board Setup Complete ${NC}"
    echo "============================================"
    echo ""
    echo "The LPC55S69-EVK board is properly configured and ready for testing."
    echo ""

    # Always show discovered probes with instructions for running tests
    if [[ ${#DISCOVERED_PROBES[@]} -gt 0 ]]; then
        echo "Run the test script with one of the discovered boards:"
        for probe in "${DISCOVERED_PROBES[@]}"; do
            echo "  HUMILITY_PROBE=$probe ./test-attestation-update.sh"
        done
        echo ""
        echo "Or set HUMILITY_PROBE in your environment:"
        for probe in "${DISCOVERED_PROBES[@]}"; do
            echo "  export HUMILITY_PROBE=$probe"
        done
        echo "  ./test-attestation-update.sh"
        echo ""
    else
        echo "Run the test script:"
        echo "  ./test-attestation-update.sh"
        echo ""
    fi
}

# Main
main() {
    echo "============================================"
    echo " LPC55S69-EVK Board Setup Check"
    echo "============================================"

    check_programs || true
    check_connection || true

    # Only continue hardware checks if we can connect
    if humility readmem 0 4 >/dev/null 2>&1; then
        check_cmpa || true
        check_bootloader || true
        check_hubris_image || true
    fi

    if ((SETUP_OK)); then
        print_success
        exit 0
    else
        print_setup_instructions
        exit 1
    fi
}

main "$@"
