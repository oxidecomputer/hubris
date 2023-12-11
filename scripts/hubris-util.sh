#!/bin/bash
set -u

RESET_SLEEP=5

fatal() {
    printf "Fatal: %s\n", "$(caller) $*"
    exit 1
}

hum_sp() {
    "${HUMILITY}" -a "${HUMILITY_ARCHIVE_SP}" -p "${SP_PROBE}" "$@"
}

hum_rot_a() {
    "${HUMILITY}" -a "${HUMILITY_ARCHIVE_ROT_A}" -p "${ROT_A_PROBE}" "$@"
}

hum_rot_b() {
    "${HUMILITY}" -a "${HUMILITY_ARCHIVE_ROT_B}" -p "${ROT_B_PROBE}" "$@"
}

hum_rot_a_erase_stage0next() {
    # This zeros the bank, it does not erase it which causes hash mismatches.
    hum_rot_a bankerase --address 0x8000 --len 0x1000
}

hum_rot_b_erase_stage0next() {
    hum_rot_b bankerase --address 0x8000 --len 0x1000
}

copy_rot_cmpa() {
    dst="${1:?Missing output file name}"
    # shellcheck disable=SC2046
    printf "%02x" $(fm read-cmpa | jq -c ".${INTERFACE}.Ok.cmpa[]") | xxd -r -p > "${dst}"
    if [[ $(stat -c '%s' "${dst}") != 512 ]]
    then
        rm -f "${dst}"
        fatal "Cannot read CMPA"
    fi
}

copy_rot_cfpa() {
    dst="${1:?Missing output file name}"
    # shellcheck disable=SC2046
    printf "%02x" $(fm read-cfpa | jq -c ".${INTERFACE}.Ok.cfpa[]") | xxd -r -p > "${dst}"
    if [[ $(stat -c '%s' "${dst}") != 512 ]]
    then
        rm -f "${dst}"
        fatal "Cannot read CFPA"
    fi
}

check_signatures() {
    BIN_DIR=$(mktemp -d)
    # shellcheck disable=SC2064
    trap "rm -fr ${BIN_DIR}" 0 1 2
    copy_rot_cfpa cfpa.bin
    copy_rot_cmpa cmpa.bin
    FAIL=()
    PASS=()
    for zip
    do
        bin="${BIN_DIR}/$(basename "$zip" .zip).bin"
        if unzip -l "${zip}" bootleby.bin
        then
            unzip -p "${zip}" bootleby.bin > "${bin}"
        else
            unzip -p "${zip}" img/final.bin > "${bin}"
        fi
        M="$(realpath cmpa.bin)"
        F="$(realpath cfpa.bin)"
        if ( cd "${HOME}/Oxide/src/lpc55_support" && cargo run --bin lpc55_sign -- verify-signed-image "${M}" "${F}" "${bin}" )
        then
            PASS+=( "${zip}" )
            green "$(caller) Signature OK $zip"
        else
            FAIL+=( "${zip}" )
            red "$(caller) Signature FAIL $zip"
        fi
    done
    if (( "${#FAIL[*]}" == 0 ))
    then
        true
    else
        false
    fi
}

# Pass the JSON without "${INTERFACE}"
# TODO: There are Rot failure cases that are not handled properly.
extract_rot_from_json() {
    J="${*:?Missing json}"

    VERSION=None
    ROT=None
    ERR=None
    if [[ "$(echo "${J}" | jq -r -c ". | keys[0]?")" == Ok ]]
    then
        VERSION="$(echo "${J}" | jq -r -c 'if ( .Ok? | keys[0]? ) == "V2" then "V2" elif ( .Ok? | keys[0]? ) == "V3" then "V3" else "Unknown" end')"
        case $VERSION in
            V2)
                OK="$(echo "${J}" | jq -r -c ".Ok.V2.rot? | keys[0]?")"
                if [[ "$OK" == Ok ]]
                then
                    ROT="$(echo "${J}" | jq -c -r ".Ok.V2.rot.Ok")"
                else
                    ROT=Unknown
                fi
                ;;
            V3)
                OK="$(echo "${J}" | jq -r -c ".Ok.V3.rot? | keys[0]?")"
                if [[ "$OK" == Ok ]]
                then
                    ROT="$(echo "${J}" | jq -c -r ".Ok.V3.rot.Ok.V3")"
                else
                    # Error
                    ROT="$(echo "${J}" | jq -c -r ".Ok.V3.rot.Err | to_entries[] | "'"\(.key)::\(.value)"')"
                fi
                ;;
            *) 
                OK=None
                error "Did not extract ROT info from '${J}'"
                fatal "JSON processing error"
                ;;
        esac
        echo "OK=$OK"
        echo "ROT=${ROT}"
    fi
    set +x
    debug "Extracted? VERSION:${VERSION} OK:${OK}"
}


# Test for the two SP APIs
# TODO: Extract RoT API as well.
get_api_versions() {
    case $(fm state | jq -r -c ".${INTERFACE} | keys[0]") in
        Ok) SP_V1=true;;
        *) SP_V1=false;;
    esac

    case $(fm state | jq -r -c ".${INTERFACE} | keys[0]") in
        Ok) SP_V2=true;;
        *) SP_V2=false;;
    esac
}

# sprot_version doesn't work this way anymore
sprot_supports_new_messages() {
    response="$(fm state -r2 2>/dev/null | jq -r -c ".${INTERFACE} | keys[]")"
    case "${response}" in
        Ok)
            true
            ;;
        Err)
            false
            ;;
        *)
            fatal "Unexpected response from faux-mgs: ${response}"
            ;;
    esac
}

# Get V3 state
get_rot_state() {
    J="$(fm state -r2 | jq -c ".${INTERFACE}")"
    extract_rot_from_json "${J}" # sets VERSION, ROT, OK, and ERR
    if [[ "$ERR" != None ]]
    then
        J="$(fm state -r1 | jq -c ".${INTERFACE}")"
        extract_rot_from_json "${J}" # sets VERSION, ROT, OK, and ERR
    fi
    J="${ROT}" # XXX extract_rot_from_json should print this for capture

    echo "VERSION=$VERSION"
    echo "J=$J"

    ACTIVE="$(echo "$J" | jq -c -r ".active")"
    # shellcheck disable=SC2046
    PERSISTENT_BOOT_PREFERENCE="$(echo "$J" | jq -c -r ".persistent_boot_preference")"
    # shellcheck disable=SC2046
    PENDING_PERSISTENT_BOOT_PREFERENCE="$(echo "$J" | jq -c -r ".pending_persistent_boot_preference")"
    # shellcheck disable=SC2046
    TRANSIENT_BOOT_PREFERENCE="$(echo "$J" | jq -c -r ".transient_boot_preference")"
    # shellcheck disable=SC2046
    SLOT_A_SHA3_256_DIGEST="$(printf "%02x" $(echo "$J" | jq -c -r ".slot_a_sha3_256_digest[]"))"
    # shellcheck disable=SC2046
    SLOT_B_SHA3_256_DIGEST="$(printf "%02x" $(echo "$J" | jq -c -r ".slot_b_sha3_256_digest[]"))"
    if [[ "$VERSION" = "V3" ]]
    then
        # shellcheck disable=SC2046
        STAGE0_SHA3_256_DIGEST="$(printf "%02x" $(echo "$J" | jq -c -r ".stage0_sha3_256_digest[]"))"
        # shellcheck disable=SC2046
        STAGE0NEXT_SHA3_256_DIGEST="$(printf "%02x" $(echo "$J" | jq -c -r ".stage0next_sha3_256_digest[]"))"

        SLOT_A_STATUS="$(echo "$J" | jq -c -r ".slot_a_status | keys[0]")" # Ok or Err
        if [[ "${SLOT_A_STATUS}" = "Ok" ]]
        then
            SLOT_A_STATUS_EPOCH="$(echo "$J" | jq ".slot_a_status.Ok.epoch")"
            SLOT_A_STATUS_VERSION="$(echo "$J" | jq ".slot_a_status.Ok.version")"
            SLOT_A_STATUS_ERR=""
        else
            SLOT_A_STATUS_EPOCH=0
            SLOT_A_STATUS_VERSION=0
            SLOT_A_STATUS_ERR="$(echo "$J" | jq ".slot_a_status.Err")"
        fi

        SLOT_B_STATUS="$(echo "$J" | jq -c -r ".slot_b_status | keys[0]")" # Ok or Err
        if [[ "${SLOT_B_STATUS}" = "Ok" ]]
        then
            SLOT_B_STATUS_EPOCH="$(echo "$J" | jq ".slot_b_status.Ok.epoch")"
            SLOT_B_STATUS_VERSION="$(echo "$J" | jq ".slot_b_status.Ok.version")"
            SLOT_B_STATUS_ERR=""
        else
            SLOT_B_STATUS_EPOCH=0
            SLOT_B_STATUS_VERSION=0
            SLOT_B_STATUS_ERR="$(echo "$J" | jq ".slot_b_status.Err")"
        fi

        STAGE0_STATUS="$(echo "$J" | jq -c -r ".stage0_status | keys[0]")" # Ok or Err
        if [[ "${STAGE0_STATUS}" = "Ok" ]]
        then
            STAGE0_STATUS_EPOCH="$(echo "$J" | jq ".stage0_status.Ok.epoch")"
            STAGE0_STATUS_VERSION="$(echo "$J" | jq ".stage0_status.Ok.version")"
            STAGE0_STATUS_ERR=""
        else
            STAGE0_STATUS_EPOCH=0
            STAGE0_STATUS_VERSION=0
            STAGE0_STATUS_ERR="$(echo "$J" | jq ".stage0_status.Err")"
        fi

        STAGE0NEXT_STATUS="$(echo "$J" | jq -c -r ".stage0next_status | keys[0]")" # Ok or Err
        if [[ "${STAGE0NEXT_STATUS}" = "Ok" ]]
        then
            STAGE0NEXT_STATUS_EPOCH="$(echo "$J" | jq ".stage0next_status.Ok.epoch")"
            STAGE0NEXT_STATUS_VERSION="$(echo "$J" | jq ".stage0next_status.Ok.version")"
            STAGE0NEXT_STATUS_ERR=""
        else
            STAGE0NEXT_STATUS_EPOCH=0
            STAGE0NEXT_STATUS_VERSION=0
            STAGE0NEXT_STATUS_ERR="$(echo "$J" | jq ".stage0next_status.Err")"
        fi
    else
        STAGE0_SHA3_256_DIGEST=""
        STAGE0NEXT_SHA3_256_DIGEST=""
        SLOT_A_STATUS=""
        SLOT_B_STATUS=""
        STAGE0NEXT_STATUS=""
        STAGE0_STATUS=""
        SLOT_A_STATUS_EPOCH=""
        SLOT_A_STATUS_VERSION=""
        SLOT_A_STATUS_ERR=""
        SLOT_B_STATUS_EPOCH=""
        SLOT_B_STATUS_VERSION=""
        SLOT_B_STATUS_ERR=""
        STAGE0_STATUS_EPOCH=""
        STAGE0_STATUS_VERSION=""
        STAGE0_STATUS_ERR=""
        STAGE0NEXT_STATUS_EPOCH=""
        STAGE0NEXT_STATUS_VERSION=""
        STAGE0NEXT_STATUS_ERR=""
    fi
    set +x
}

# Discover the active bank
get_active_rot_bank() {
    fm state |
        jq -r -c ".${INTERFACE}.Ok | if ( .V2.rot.Ok.active ) then .V2.rot.Ok.active else .V3.rot.Ok.active end"
    }

print_rot_state() {
    printf "ACTIVE:%s PENDING_PERSISTENT:%s PERSISTENT:%s TRANSIENT:%s\n" \
        "${ACTIVE}" "${PENDING_PERSISTENT_BOOT_PREFERENCE}" \
        "${PERSISTENT_BOOT_PREFERENCE}" "${TRANSIENT_BOOT_PREFERENCE}"

    printf "A:           %s %s,%s/%s\n" "${SLOT_A_SHA3_256_DIGEST}" \
        "${SLOT_A_STATUS_EPOCH}" "${SLOT_A_STATUS_VERSION}" \
        "${SLOT_A_STATUS_ERR}"

    printf "B:           %s %s,%s/%s\n" "${SLOT_B_SHA3_256_DIGEST}" \
        "${SLOT_B_STATUS_EPOCH}" "${SLOT_B_STATUS_VERSION}" \
        "${SLOT_B_STATUS_ERR}"

    printf "STAGE0:      %s %s,%s/%s\n" "${STAGE0_SHA3_256_DIGEST}" \
        "${STAGE0_STATUS_EPOCH}" "${STAGE0_STATUS_VERSION}" \
        "${STAGE0_STATUS_ERR}"

    printf "STAGE0NEXT: %s %s,%s/%s\n" "${STAGE0NEXT_SHA3_256_DIGEST}" \
        "${STAGE0NEXT_STATUS_EPOCH}" "${STAGE0NEXT_STATUS_VERSION}" \
        "${STAGE0NEXT_STATUS_ERR}"
}

predict() {
    cmpa="${1:?Missing path to CMPA file}"
    shift
    cfpa="${1:?Missing path to CFPA file}"
    shift
    bin="${1:?Missing path to bootleby.bin file}"
    shift
    lpc55_sign verify-signed-image "${cmpa}" "${cfpa}" "${bin}"
}

read_rot_caboose_var() {
    fm read-component-caboose rot --slot "${1:?Missing slot}" "${2:?Missing caboose var}" |
        jq -c -r ".${INTERFACE}.Ok.value"
}

# Get caboose vars into env from specified slot: CABOOSE_{GITC,BORD,NAME,VERS}
read_rot_caboose() {
    slot="${1:?Missing slot}"
    # shellcheck disable=SC2034
    CABOOSE_GITC="$(read_rot_caboose_var "${slot}" GITC)"
    # shellcheck disable=SC2034
    CABOOSE_BORD="$(read_rot_caboose_var "${slot}" BORD)"
    # shellcheck disable=SC2034
    CABOOSE_NAME="$(read_rot_caboose_var "${slot}" NAME)"
    # shellcheck disable=SC2034
    CABOOSE_VERS="$(read_rot_caboose_var "${slot}" VERS)"
}

log() {
    LOGFILE="${1:?Missing log file}"
    rm -f "${LOGFILE}"
    touch "${LOGFILE}"
}

# Exit if one of the listed paths is not readable.
check_readable() {
    for x
    do
        [[ -r $x ]] || fatal "File is not readable: $x"
    done
}

color() (
code="${1:?Missing color code}"
shift
printf "\033[${code}m%s\033[m\n" "$*"
if [[ -n "${LOGFILE:-}" ]]
then
    printf "%s\n" "$*" >> "${LOGFILE}"
fi
)


blue() {
    color "44" "$*"
}

green() {
    color "42" "$*"
}

red() {
    color "41" "$*"
}

cyan() {
    color "46" "$*"
}

yellow() {
    color "43" "$*"
}

magenta() {
    color "45" "$*"
}

white() {
    color "47;30" "$*"
}

black() {
    color "40" "$*"
}

section() {
    white "$(caller) $*"
}

fact() {
    blue "$(caller) $*"
}

action() {
    magenta "$(caller) $*"
}

error() {
    red "$(caller) $*"
}

success() {
    green "$(caller) $*"
}

debug() {
    yellow "$(caller) $*"
}

reset_sp_and_sleep() {
    fm reset
    sleep "${1:-$RESET_SLEEP}"
}

reset_rot_and_sleep() {
    fm reset-component rot
    sleep "${1:-$RESET_SLEEP}"
}

fwid_from_zip() {
	[[ -x "${ROT_FWID}" ]] || fatal 'No rot-fwid executable'
    "${ROT_FWID}" -d sha3-256 "${1:?Missing file}" | cut -d' ' -f3
}

image_gitc() {
    image="${1:?Missing zip file}"
    gitc="$(hubedit  --archive "${image}" read-caboose | sed -n -e '/GITC/{N;s/\n//;}' -e 's=[,"(\[]==g' -e '/^.*GITC */s===p' -)"
    echo "${gitc}"
}

reset_sp() {
    if [[ "$(fm reset | jq -c -r ".${INTERFACE} | keys[0]")" == "Ok" ]]
    then
        true
    else
        false
    fi
}


fm() {
    "${FAUX_MGS}" --log-level=CRITICAL --json pretty "$@"
}
