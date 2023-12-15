#!/bin/bash
set -u

RESET_SLEEP=5

fatal() {
    printf "Fatal: %s\n", "$(caller) $*"
    exit 1
}

IMAGE_A_BASE=$(( 0x00010000 ))
IMAGE_A_END=$(( 0x00050000 ))
IMAGE_A_LEN=$(( IMAGE_A_END - IMAGE_A_BASE ))
IMAGE_B_BASE=$(( 0x00050000 ))
IMAGE_B_END=$(( 0x00090000 ))
IMAGE_B_LEN=$(( IMAGE_B_END - IMAGE_B_BASE ))
IMAGE_STAGE0_BASE=$(( 0x00000000 ))
IMAGE_STAGE0_END=$(( 0x00008000 ))
IMAGE_STAGE0_LEN=$(( IMAGE_STAGE0_END - IMAGE_STAGE0_BASE ))
IMAGE_STAGE0NEXT_BASE=$(( 0x00008000 ))
IMAGE_STAGE0NEXT_END=$(( 0x00010000 ))
IMAGE_STAGE0NEXT_LEN=$(( IMAGE_STAGE0NEXT_END - IMAGE_STAGE0NEXT_BASE ))

power_state() {
    "${FAUX_MGS}" --log-level=CRITICAL --json pretty state |
        jq -c -r ".${INTERFACE}.Ok.V2.power_state"
}

rot_bankerase() {
    slot="${1:?Missing slot name}"
    shift
    pages="${1:?Missing number of pages or 'all'}"
    shift
    # match lower case version of $slot
    case "${slot,,}" in
    a)
        BASE=$IMAGE_A_BASE
        _END=$IMAGE_A_END
        LEN=$IMAGE_A_LEN
        ;;
    b)
        BASE=$IMAGE_B_BASE
        _END=$IMAGE_B_END
        LEN=$IMAGE_B_LEN
        ;;

    stage0)
        BASE=$IMAGE_STAGE0_BASE
        _END=$IMAGE_STAGE0_END
        LEN=$IMAGE_STAGE0_LEN
        ;;

    stage0next)
        BASE=$IMAGE_STAGE0NEXT_BASE
        _END=$IMAGE_STAGE0NEXT_END
        LEN=$IMAGE_STAGE0NEXT_LEN
        ;;
    *)
        fatal "Unknown slot name: $slot. Use a, b, stage0, or stage0next"
        ;;
    esac
    if [[ "${pages,,}" != "all" ]]
    then
        if ! [[ "$pages" =~ ^[0-9]+$ ]]
        then
            fatal "Non-numeric number of pages: $pages"
        fi
        if (( pages * 512 < LEN ))
        then
            LEN=$(( pages * 512 ))
        fi
    fi
    # Note that the bankerase command doesn't care about the active bank or
    # the image contents but humility needs an archive.
    # We provide the A image from the Hubris master branch.
    "${HUMILITY}" -p "${ROT_PROBE}" -a "${MASTER_ROT_A_ZIP}" bankerase --address $BASE --len $LEN
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
        if lpc55_sign verify-signed-image "${M}" "${F}" "${bin}"
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

# Check for rot-boot-info being supported
# TODO: Extract RoT API as well.
get_apis_supported() {
    J="$(fm rot-boot-info -v3 2>/dev/null | jq -r -c ".${INTERFACE}")"
    echo "$(caller) J=$J"
    case $(echo "$J"| jq -r -c ". | keys[0]") in
        Ok)
            SP_RBI_SUPPORT=true
            ROT_RBI_SUPPORT=true
            ;; # SP and RoT support rot-boot-info
        Err)
            case "$(echo "$J" | jq -r -c ".Err")" in
                "Error response from SP: bad request"*) # SP doesn't know the new command
                    SP_RBI_SUPPORT=false
                    ROT_RBI_SUPPORT=false # Actually unknown
                    ;;
                "Error response from SP: sprot: failed to deserialize"*) # RoT doesn't know the new command
                    SP_RBI_SUPPORT=true
                    ROT_RBI_SUPPORT=false
                ;;
            esac
            ;; # SP supports it but RoT does not
        *)
            SP_RBI_SUPPORT=false
            ROT_RBI_SUPPORT=unknown
            ;;  # faux-mgs or SP do not support, RoT unknown.
    esac
    echo "SP_RBI_SUPPORT=$SP_RBI_SUPPORT ROT_RBI_SUPPORT=$ROT_RBI_SUPPORT"
}

# sprot_supports_new_messages()

# Get V3 state
get_rot_state() {
    J="$(fm rot-boot-info | jq -c ".${INTERFACE}")"
    # Ok = SP and ROT support it
    # Err="sprot: failed to deserialze" = Only SP supports
    # Err="bad request" = No support from SP
    # no output: faux-mgs is the old version
    RESPONSE="$(echo "$J" | jq -c -r ". | keys[0] ")"
    case "${RESPONSE}" in
        "")
            fatal "faux-mgs does not support rot-boot-info command"
            ;;
        "Err") # No rot-boot-info command support, use state command.
            J="$(fm state | jq -c ".${INTERFACE}")"
            ok="$(echo "$J" | jq -c -r "keys[0]")"
            case "$ok" in
                Ok)
                    # Extract the RoT part
                    J="$(echo "$J" | jq -c ".Ok.V2.rot.Ok")"
                    VERSION=V2
                    ;;
                *)
                    fatal "Error requesting sp state"
                    ;;
            esac
            ;;
        "Ok")
            J="$(echo "$J" | jq -c ".Ok.V3")"
            VERSION=V3
            ;;
    esac
    echo "VERSION=$VERSION"
    echo "J=$J"

    ACTIVE="$(echo "$J" | jq -c -r ".active")"
    # shellcheck disable=SC2046
    PERSISTENT_BOOT_PREFERENCE="$(echo "$J" | jq -c -r ".persistent_boot_preference")"
    # shellcheck disable=SC2046
    PENDING_PERSISTENT_BOOT_PREFERENCE="$(echo "$J" | jq -c -r ".pending_persistent_boot_preference")"
    # shellcheck disable=SC2046
    TRANSIENT_BOOT_PREFERENCE="$(echo "$J" | jq -c -r ".transient_boot_preference")"
    if [[ "$VERSION" = "V3" ]]
    then
        # shellcheck disable=SC2046
        SLOT_A_FWID="$(printf "%02x" $(echo "$J" | jq -c -r ".slot_a_fwid.Sha3_256[]"))"
        # shellcheck disable=SC2046
        SLOT_B_FWID="$(printf "%02x" $(echo "$J" | jq -c -r ".slot_b_fwid.Sha3_256[]"))"
        # shellcheck disable=SC2046
        STAGE0_FWID="$(printf "%02x" $(echo "$J" | jq -c -r ".stage0_fwid.Sha3_256[]"))"
        # shellcheck disable=SC2046
        STAGE0NEXT_FWID="$(printf "%02x" $(echo "$J" | jq -c -r ".stage0next_fwid.Sha3_256[]"))"

        SLOT_A_STATUS="$(echo "$J" | jq -c -r ".slot_a_status | keys[0]")" # Ok or Err
        if [[ "${SLOT_A_STATUS}" = "Ok" ]]
        then
            SLOT_A_STATUS_ERR=""
        else
            SLOT_A_STATUS_ERR="$(echo "$J" | jq ".slot_a_status.Err")"
        fi

        SLOT_B_STATUS="$(echo "$J" | jq -c -r ".slot_b_status | keys[0]")" # Ok or Err
        if [[ "${SLOT_B_STATUS}" = "Ok" ]]
        then
            SLOT_B_STATUS_ERR=""
        else
            SLOT_B_STATUS_ERR="$(echo "$J" | jq ".slot_b_status.Err")"
        fi

        STAGE0_STATUS="$(echo "$J" | jq -c -r ".stage0_status | keys[0]")" # Ok or Err
        if [[ "${STAGE0_STATUS}" = "Ok" ]]
        then
            STAGE0_STATUS_ERR=""
        else
            STAGE0_STATUS_ERR="$(echo "$J" | jq ".stage0_status.Err")"
        fi

        STAGE0NEXT_STATUS="$(echo "$J" | jq -c -r ".stage0next_status | keys[0]")" # Ok or Err
        if [[ "${STAGE0NEXT_STATUS}" = "Ok" ]]
        then
            STAGE0NEXT_STATUS_ERR=""
        else
            STAGE0NEXT_STATUS_ERR="$(echo "$J" | jq ".stage0next_status.Err")"
        fi
    else
        # shellcheck disable=SC2046
        SLOT_A_FWID="$(printf "%02x" $(echo "$J" | jq -c -r ".slot_a_sha3_256_digest[]"))"
        # shellcheck disable=SC2046
        SLOT_B_FWID="$(printf "%02x" $(echo "$J" | jq -c -r ".slot_b_sha3_256_digest[]"))"
        STAGE0_FWID=""
        STAGE0NEXT_FWID=""
        SLOT_A_STATUS=""
        SLOT_B_STATUS=""
        STAGE0NEXT_STATUS=""
        STAGE0_STATUS=""
        SLOT_A_STATUS_ERR=""
        SLOT_B_STATUS_ERR=""
        STAGE0_STATUS_ERR=""
        STAGE0NEXT_STATUS_ERR=""
    fi
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

    printf "A:           %s %s\n" "${SLOT_A_FWID}" \
        "${SLOT_A_STATUS_ERR}"

    printf "B:           %s %s\n" "${SLOT_B_FWID}" \
        "${SLOT_B_STATUS_ERR}"

    printf "STAGE0:      %s %s\n" "${STAGE0_FWID}" \
        "${STAGE0_STATUS_ERR}"

    printf "STAGE0NEXT: %s %s\n" "${STAGE0NEXT_FWID}" \
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

reset_rot_and_poll_ready() {
    fm reset-component rot | jq -c -r ".${INTERFACE}.Ok.ack"
    poll_rot_ready
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

# Update and run a new RoT image in the specified bank.
# The supplied image must match bank layout (0=A, 1=B).
# Do not optimize to take advantage of an exising matching
# image in the alternate bank.
# We will eventually get both banks to the same version to
# complete an update.
update_rot_hubris() {
    IMAGE_ZIP="${1:?Missing ROT Hubris image}"
    shift
    IMAGE_BANK="${1:?Missing ROT Hubris image bank number}"
    shift

    section "Update RoT with update-stage0 branch image."
    action fm update rot "${IMAGE_BANK}" "${IMAGE_ZIP}"
    if ! fm update rot "${IMAGE_BANK}" "${IMAGE_ZIP}"
    then
        fatal Failed to update RoT Hubris image to "${IMAGE_ZIP}"
    fi

    # TODO: Use transient boot selection to test new image fitness. Only persist if fit.

    # After fitness test, commit to new version
    action "fm component-active-slot -s ${IMAGE_BANK}" -p rot
    fm component-active-slot -s "${IMAGE_BANK}" -p rot
    action "fm reset-component rot"
    # RoT is not ready immediately after reset returns.
    reset_rot_and_poll_ready

    section "Check the RoT version to make sure it worked."
    ACTIVE=$(get_active_rot_bank)
    fact "Active RoT bank is ${ACTIVE}"
    if [[ "${IMAGE_BANK}" == 0 ]]
    then
        if [[ "${ACTIVE}" == "A" ]]
        then
            true
        else
            error "Wrong active RoT bank, IMAGE_BANK=${IMAGE_BANK}, ACTIVE=${ACTIVE}"
            false
        fi
    else
        if [[ "${ACTIVE}" == "B" ]]
        then
            true
        else
            error "Wrong active RoT bank, IMAGE_BANK=${IMAGE_BANK}, ACTIVE=${ACTIVE}"
            false
        fi
    fi
}


fm() {
    "${FAUX_MGS}" --log-level=CRITICAL --json pretty "$@"
}

# Set ACTIVE, US0_ROT_ZIP and ROT_UPDATE_BANK to reflect the US0 image to
# send to the RoT.
select_next_rot_image() {
    IMAGE_A="${1:?Missing RoT Image A path}"
    shift
    IMAGE_B="${1:?Missing RoT Image B path}"
    shift
    ROT_UPDATE_BANK=999 # Invalid value
    ROT_ZIP=None
    ACTIVE=$(get_active_rot_bank)
    case "${ACTIVE}" in
        A)
            # shellcheck disable=SC2153
            ROT_ZIP="${IMAGE_B}"
            ROT_UPDATE_BANK=1
            ;;
        B)
            # shellcheck disable=SC2153
            ROT_ZIP="${IMAGE_A}"
            ROT_UPDATE_BANK=0
            ;;
        *)
            fatal Bank "${ACTIVE}" is unknown
            ;;
    esac
    case $ROT_UPDATE_BANK in
        1|0)
            ;;
        *) fatal bug
            ;;
    esac
}

# A master branch image will not suport the new status message.
is_rot_boot_info_supported_by_sp() {
    get_apis_supported
    $SP_RBI_SUPPORT
}

is_rot_boot_info_supported_by_rot() {
    get_apis_supported
    $SP_RBI_SUPPORT && $ROT_RBI_SUPPORT
}

# The Hubris Archive ID is an FNV hash of the output sections of the image,
# kconfig, and "allocations" for an image. It's not clear on a cursory reading
# of the code if this hash is reproducable given inputs for two successive
# builds of Hubris. FNV is a fast hash that is not cryptographically hard
# for use cases where that is appropriate.
# False positives and negatives are not likely, but not impossible.
sp_v2_archive_id() {
    # shellcheck disable=SC2046
    printf "%02x" $("${FAUX_MGS}" --log-level=CRITICAL --json pretty state |
        jq -c -r ".${INTERFACE}.Ok.V2.hubris_archive_id[]" )
}

sp_v3_archive_id() {
    # shellcheck disable=SC2046
    printf "%02x" $("${FAUX_MGS}" --log-level=CRITICAL --json pretty state |
        jq -c -r ".${INTERFACE}.Ok.V3.hubris_archive_id[]" )
}

update_sp() {
    OLD_IMAGE="${1:?Missing old image path}"
    shift
    NEW_IMAGE="${1:?Missing new image path}"
    shift
    [[ -r "${OLD_IMAGE}" ]] || "fatal Cannot read ${OLD_IMAGE}"
    [[ -r "${NEW_IMAGE}" ]] || "fatal Cannot read ${NEW_IMAGE}"
    action "${FAUX_MGS} --log-level=DEBUG update sp 0 ${NEW_IMAGE}"
    if fm update sp 0 "${NEW_IMAGE}"
    then
        fact faux-mgs success
    else
        error faux-mgs failed
        fatal "cannot update SP to ${NEW_IMAGE}"
    fi
    action "fm reset"
    if ! reset_sp
    then
        error "Failed to reset SP"
        fatal "Could not reset SP"
    fi

    section "Check the SP version to make sure it worked."

    (( LIMIT=5 ))
    while ! SP_GITC="$(fm read-caboose GITC | jq -c -r ".${INTERFACE}.Ok.value")"
    do
        (( LIMIT -= 1 ))
        if (( LIMIT <= 0 ))
        then
            fatal Cannot get SP GITC
        fi
        sleep 2
    done
    fact "SP_GITC=${SP_GITC}"

    OLD_GITC="$(image_gitc "${OLD_IMAGE}")"
    NEW_GITC="$(image_gitc "${NEW_IMAGE}")"
    fact OLD_GITC="${OLD_GITC}"
    fact NEW_GITC="${NEW_GITC}"

    if [[ "${SP_GITC}" == "${NEW_GITC}" ]]
    then
        success "SP is running expected version"
        true
    else
        error "SP is not running expected version"
        false
    fi
}

update_stage0next() {
    local install_zip
    INSTALL_ZIP="${1:?Missing stage0next install image path}"
    INSTALL_FWID="$(fwid_from_zip "${INSTALL_ZIP}")"

    section "Installing ${INSTALL_ZIP} to stage0next"
    action fm update stage0 1 "${INSTALL_ZIP}"
    if fm update stage0 1 "${INSTALL_ZIP}"
    then
        success "Installed in stage0next"
    else
        error "Failed to install"
    fi

    section "Reset RoT to evaluate stage0next"
    reset_rot_and_poll_ready
    get_rot_state
    if [[ "${STAGE0NEXT_FWID}" != "${INSTALL_FWID}" ]]
    then
        error "stage0next did not update: reading:${STAGE0NEXT_FWID} != goal:$INSTALL_FWID"
        set | grep DIGEST
        false
    else
        success "stage0next updated: reading==goal (${INSTALL_FWID})"
        true
    fi
}

persist_to_stage0_reset_and_test() {
    GOAL_FWID="${1:?Missing goal FWID}"
    section "Persist stage0next to stage0"
    if ! fm component-active-slot stage0 --set 1 -p
    then
        error Persist operation failed
        false
    else
        success Persist operation succeeded
        section reboot to new stage0 image
        reset_rot_and_poll_ready
        get_rot_state
        if [[ "${STAGE0NEXT_FWID}" != "${GOAL_FWID}" ]]
        then
            error "Intended stage0 image is not present"
            false
        else
            success "Successfully installed $INSTALL_IMAGE_ZIP"
            true
        fi
    fi
}

poll_rot_ready() {
    (( limit = 20 ))
    while :
    do
        result="$(fm state | jq -r -c ".${INTERFACE}.Ok.V2.rot | keys[0]")"
        if [[ "$result" = Ok ]]
        then
            break
        fi
        (( ( limit -= 1 ) < 0 )) && fatal Timeout
        sleep 1
    done
}
