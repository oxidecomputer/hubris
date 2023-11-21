#!/bin/bash

set -e
set -u

_PROG=$(basename "$0")
PROG_DIR="$(dirname "$(realpath "$0")")"

source "${PROG_DIR}/hubris-util.sh"
# set +e


# Prove that there is a faux-mgs method from going from master branch to
# update-stage0 branch and then be able to update stage0.
#
# humility is used to reset to initial conditions and to verify successful
# update.
# Otherwise, use faux-mgs to mimic the control plane.
#
# Note: Build all of the needed products in the mainline and branch under test
# before running this script.

# A file containing the same variable settings as ${DEFAULT_CONFIG} can be
# passed as the first argument to this scripe.

# Note that most of the functions in this script are executed in the primary
# shell and as such affect global state. This is a cheap and sloppy way
# to return values used later.
#
# Shell scripts are requirements documents, not products.

# shellcheck disable=SC2016
DEFAULT_CONFIG='
[[ $(uname -n) == "voidstar" ]] || fatal "Default configuration for $(uname -n) is not appropriate."
INTERFACE="enp65s0f0"
SP_PROBE="usb-1"
ROT_PROBE="usb-0"
MASTER_TARGET="${HOME}/Oxide/src/hubris/master/target"
US0_TARGET="${HOME}/Oxide/src/hubris/update-stage0/target"
FAUX_MGS="${HOME}/bin/faux-mgs"
HUMILITY="${HOME}/bin/humility"

# Initial images to be flashed to the SP and RoT.
MASTER_SP_ZIP="${MASTER_TARGET}/gimletlet/dist/default/build-gimletlet-image-default.zip"
MASTER_ROT_A_ZIP="${MASTER_TARGET}/rot-carrier/dist/a/build-rot-carrier-image-a.zip"
MASTER_ROT_B_ZIP="${MASTER_TARGET}/rot-carrier/dist/b/build-rot-carrier-image-b.zip"

# Images for "update-stage0"
US0_SP_ZIP="${US0_TARGET}/gimletlet/dist/default/build-gimletlet-image-default.zip"
US0_ROT_A_ZIP="${US0_TARGET}/rot-carrier/dist/a/build-rot-carrier-image-a.zip"
US0_ROT_B_ZIP="${US0_TARGET}/rot-carrier/dist/b/build-rot-carrier-image-b.zip"

BOOTLEBY_LATEST_ZIP="${HOME}/Oxide/src/embootleby/bundles/rot-carrier-bart-unlocked.zip"
BOOTLEBY_NEXT_ZIP="${HOME}/Oxide/src/bootleby/board/bootleby-rot-carrier.zip"

BOOTLEBY_OLD1_ZIP="${HOME}/Oxide/src/embootleby/restore/master/bundles/rot-carrier-bart-unlocked.zip"
BOOTLEBY_OLD2_ZIP="${HOME}/Oxide/src/embootleby/restore/2023-06-02_4ca595d/bundles/rot-carrier-bart-unlocked.zip"

BROKEN_STAGE0=()
BROKEN_STAGE0+=( "${BOOTLEBY_OLD1_ZIP}" )
BROKEN_STAGE0+=( "${BOOTLEBY_OLD2_ZIP}" )

WORKING_STAGE0=()
WORKING_STAGE0+=( "${BOOTLEBY_LATEST_ZIP}" )
WORKING_STAGE0+=( "${BOOTLEBY_NEXT_ZIP}" )


ALL_ROT_IMAGES=()
ALL_ROT_IMAGES+=( "${MASTER_ROT_A_ZIP}" )
ALL_ROT_IMAGES+=( "${MASTER_ROT_B_ZIP}" )
ALL_ROT_IMAGES+=( "${US0_ROT_A_ZIP}" )
ALL_ROT_IMAGES+=( "${US0_ROT_B_ZIP}" )
ALL_ROT_IMAGES+=( "${BOOTLEBY_LATEST_ZIP}" )
ALL_ROT_IMAGES+=( "${BOOTLEBY_NEXT_ZIP}" )
ALL_ROT_IMAGES+=( "${BOOTLEBY_OLD1_ZIP}" )
ALL_ROT_IMAGES+=( "${BOOTLEBY_OLD2_ZIP}" )

ALL_IMAGES=()
ALL_IMAGES+=( "${ALL_ROT_IMAGES[@]}" )
ALL_IMAGES+=( "${MASTER_SP_ZIP}" )
ALL_IMAGES+=( "${US0_SP_ZIP}" )
'

CONFIG="${1:-$DEFAULT_CONFIG}"

if [[ -r "${CONFIG}" ]]
then
    #shellcheck disable=SC1090
    source "${CONFIG}"
else
    eval "${CONFIG}"
fi
set | grep "_ZIP=${HOME}"

check_dependencies() {
    # shellcheck disable=SC2153
    check_readable "${ALL_IMAGES[@]}"

    DEPEND=( )
    DEPEND+=( rot-image-hash )
    DEPEND+=( faux-mgs )
    DEPEND+=( jq )
    DEPEND+=( unzip )
    DEPEND+=( stat )
    missing=( )

    for prog in "${DEPEND[@]}"
    do
        which "${prog}" 1>/dev/null 2>/dev/null || missing+=( "${prog}" )
    done
    if (( "${#missing[@]}" > 0 ))
    then
        fatal "Support programs are missing: ${missing[*]}"
    else
        fact "Script dependencies are available"
    fi
}

section Verify that script dependencies and test images are present.
check_dependencies

section Show the FWID and GITC values from each image
for image in "${ALL_IMAGES[@]}"
do
    fwid="$(fwid_from_zip "${image}")"
    gitc="$(image_gitc "${image}")"
    fact "$(printf "Image: %s\n\tFWID: %s\n\tGITC: %s\n" "${image}" "${fwid}" "${gitc}")"
done

power_state() {
    faux-mgs --log-level=CRITICAL --json pretty state -r1 |
        jq -c -r ".${INTERFACE}.Ok.V2.power_state"
}

# We don't want to update the SP or RoT when a system is powered up
if [[ "$(power_state)" != "A2" ]]
then
    fatal "Device must be in power_state A2"
fi

# No inadvertent humility parameters through env
unset HUMILITY_ARCHIVE
unset HUMILITY_ENVIRONMENT
unset HUMILITY_PROBE

initialize_test() {
    section Use humility to install master branch images
    action "Flash SP with master branch image"
    action "${HUMILITY} --archive ${MASTER_SP_ZIP} -p ${SP_PROBE} flash"
    ${HUMILITY} --archive "${MASTER_SP_ZIP}" -p "${SP_PROBE}" flash 2>&1 |
        grep -q 'already flashed' && echo Already Flashed SP

    ${HUMILITY} -p "${SP_PROBE}" reset
    sleep 10

    action "Flash RoT A with master branch image using Humility"
    action "${HUMILITY} --archive ${MASTER_ROT_A_ZIP} -p ${ROT_PROBE} flash"
    ${HUMILITY} --archive "${MASTER_ROT_A_ZIP}" -p "${ROT_PROBE}" flash 2>&1 |
        grep -q 'already flashed' && echo Already Flashed ROT A
    sleep 3

    action "Flash RoT B with master branch image using Humility"
    action "${HUMILITY} --archive ${MASTER_ROT_B_ZIP} -p ${ROT_PROBE} flash"
    ${HUMILITY} --archive "${MASTER_ROT_B_ZIP}" -p "${ROT_PROBE}" flash 2>&1 |
        grep -q 'already flashed' && echo Already Flashed ROT B

    action "Last humility operation, reset RoT"
    ${HUMILITY} -p "${ROT_PROBE}" reset
    sleep 10

    action "Persist RoT image A using faux-mgs"
    action "fm component-active-slot -s 0 -p rot"
    OK="$(fm component-active-slot -s 0 -p rot | jq -r -c ".${INTERFACE} | keys[0]")"
    [[ "${OK}" == "Ok" ]] || fatal "Failed to set persistent Hubris image."
    action "Reset RoT"
    reset_rot_and_sleep 10

    ACTIVE=$(get_active_rot_bank)
    case "${ACTIVE}" in
        A) success "Master branch images are running with RoT booting from bank A.";;
        *) error "Rot active image is ${ACTIVE}"
            fatal "Rot active image is not A"
            ;;
    esac
}

initialize_test

# A master branch image will not suport the new status message.
new_rot_status_supported() {
    ERROR="$(faux-mgs --log-level=CRITICAL --json pretty state -r2 | jq -c -r ".${INTERFACE}.Err")"
    [[ -n "${ERROR}" ]]
}

if ! new_rot_status_supported
then
    fatal "New SP state data should not be available"
fi

# The SP and RoT under test now have Hubris master branch images that do not
# support stage0 update and do not expose the FWID hashes unless there
# is a valid image for the respective Hubris image bank.
#

# "US0" is used as a prefix for the "Update Stage0" branch.

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

select_next_rot_image "${US0_ROT_A_ZIP}" "${US0_ROT_B_ZIP}"

# We should be at initial conditions with RoT Hubris bank A active, 
# If that's not the case, then bail out.
if [[ $ACTIVE != A ]]
then
    error "Unexpected initial conditions, active RoT image is $ACTIVE"
    fatal "Unexpected initial conditions, active RoT image is $ACTIVE"
else
    success "Active RoT image is $ACTIVE"
fi

# Check for supported SpRot state message support
get_api_versions
"${SP_V1}" || fatal "Cannot communicate with SP"
"${SP_V2}" && fatal "Master branch should not be supporting V2 yet"

# The Hubris Archive ID is an FNV hash of the output sections of the image,
# kconfig, and "allocations" for an image. It's not clear on a cursory reading
# of the code if this hash is reproducable given inputs for two successive
# builds of Hubris. FNV is a fast hash that is not cryptographically hard
# for use cases where that is appropriate.
# False positives and negatives are not likely, but not impossible.
sp_v2_archive_id() {
    # shellcheck disable=SC2046
    printf "%02x" $(faux-mgs --log-level=CRITICAL --json pretty state |
        jq -c -r ".${INTERFACE}.Ok.V2.hubris_archive_id[]" )
}

sp_v3_archive_id() {
    # shellcheck disable=SC2046
    printf "%02x" $(faux-mgs --log-level=CRITICAL --json pretty state |
        jq -c -r ".${INTERFACE}.Ok.V3.hubris_archive_id[]" )
}

fact "SP image hubris_archive_id: $(sp_v2_archive_id)"

section "Use faux-mgs to update SP with update-stage0 branch image"

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

if ! update_sp "${MASTER_SP_ZIP}" "${US0_SP_ZIP}"
then
    fatal "Was not able to update SP to ${US0_SP_ZIP}"
fi
get_api_versions
if "${SP_V2}"
then
    success SP supports new API
else
    fatal "SP does not support new API"
fi


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
    fm reset-component rot
    # RoT is not ready immediately after reset returns.
    # TODO: This is supposed to be handled in reset and reset-component for
    # their respective target components.
    sleep 3
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

if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

section Check image signatures against installed RoT CMPA/CFPA.

if check_signatures "${ALL_ROT_IMAGES[@]}"
then
    success "All signatures are good"
else
    error "Some signatures do not verify against CFPA/CMPA"
    # shellcheck disable=SC2153
    for pass in "${PASS[@]}"; do success "${pass}"; done
    # shellcheck disable=SC2153
    for fail in "${FAIL[@]}"; do error "${fail}"; done
    fatal "Cannot test with incompatible binaries"
fi

section "Get a hash of the installed bootleby version."
get_rot_state
# These vars are now set/refreshed from V3 state:
#  ACTIVE, PENDING_PERSISTENT_BOOT_PREFERENCE, PERSISTENT_BOOT_PREFERENCE,
#  TRANSIENT_BOOT_PREFERENCE, SLOT_A_SHA3_256_DIGEST, SLOT_B_SHA3_256_DIGEST,
#  STAGE0_SHA3_256_DIGEST, STAGE0_NEXT_SHA3_256_DIGEST, SLOT_A_STATUS_EPOCH,
#  SLOT_A_STATUS_VERSION, SLOT_A_STATUS_ERR, SLOT_B_STATUS_EPOCH,
#  SLOT_B_STATUS_VERSION, SLOT_B_STATUS_ERR, STAGE0_STATUS_EPOCH,
#  STAGE0_STATUS_VERSION, STAGE0_STATUS_ERR, STAGE0_NEXT_STATUS_EPOCH,
#  STAGE0_NEXT_STATUS_VERSION, STAGE0_NEXT_STATUS_ERR
fact "Installed stage0 fwid=${STAGE0_SHA3_256_DIGEST}"
fact "Installed stage0next fwid=${STAGE0_NEXT_SHA3_256_DIGEST}"

section "Show that stage0 can be updated by installing a different image"

select_different_stage0() {
    BEGIN_STAGE0_FWID="${STAGE0_SHA3_256_DIGEST}"
    BEGIN_STAGE0_NEXT_FWID="${STAGE0_NEXT_SHA3_256_DIGEST}"
    _CONTENTS=( "${BEGIN_STAGE0_FWID}" "${BEGIN_STAGE0_NEXT_FWID}" )
    INSTALL_IMAGE_ZIP=None
    INSTALL_IMAGE_FWID=None
    INSTALL_IMAGE_ZIP="None found"
    INSTALL_IMAGE_FWID="None found"
    for image in "${WORKING_STAGE0[@]}"
    do
        fwid="$(fwid_from_zip "${image}")"
        if ! [[ "${BEGIN_STAGE0_FWID}" == "${fwid}" ]]
        then
            INSTALL_IMAGE_ZIP="${image}"
            INSTALL_IMAGE_FWID="${fwid}"
            break
        fi
    done
    if [[ "${INSTALL_IMAGE_FWID}" == "None found" ]]
    then
        fatal "No unique stage0 image is available for testing"
    fi
}

select_different_stage0 # fail or set INSTALL_IMAGE_ZIP and INSTALL_IMAGE_FWID

update_stage0next() {
    local install_zip
    INSTALL_ZIP="${1:?Missing stage0next install image path}"
    INSTALL_FWID="$(fwid_from_zip "${install_zip}")"

    section "Installing ${install_zip} to stage0next"
    action fm update rot 3 "${INSTALL_ZIP}"
    if fm update rot 3 "${INSTALL_ZIP}"
    then
        success "Installed in stage0next"
    else
        error "Failed to install"
    fi

    section "Reset RoT to evaluate stage0next"
    reset_rot_and_sleep
    get_rot_state
    if [[ "${STAGE0_NEXT_SHA3_256_DIGEST}" != "${INSTALL_FWID}" ]]
    then
        error "stage0next did not update: reading:${STAGE0_NEXT_SHA3_256_DIGEST} != goal:$INSTALL_FWID"
        set | grep DIGEST
        false
    else
        success "stage0next updated: reading==goal (${INSTALL_FWID})"
        true
    fi
}

if update_stage0next "${INSTALL_IMAGE_ZIP}"
then
    success "stage0next updated: reading==goal (${INSTALL_FWID})"
else
    fatal "update stage0next failed"
fi

persist_to_stage0_reset_and_test() {
    GOAL_FWID="${1:?Missing goal FWID}"
    section "Persist stage0next to stage0"
    if ! fm component-active-slot rot --set 3 -p
    then
        error Persist operation failed
        false
    else
        success Persist operation succeeded
        section reboot to new stage0 image
        reset_rot_and_sleep
        get_rot_state
        if [[ "${STAGE0_NEXT_SHA3_256_DIGEST}" != "${GOAL_FWID}" ]]
        then
            error "Intended stage0 image is not present"
            false
        else
            success "Successfully installed $INSTALL_IMAGE_ZIP"
            true
        fi
    fi
}

if ! persist_to_stage0_reset_and_test "${INSTALL_IMAGE_FWID}"
then
    fatal "failed to install $INSTALL_IMAGE_ZIP"
fi

section "Update alternate RoT Hubris image to complete the transition to the new image"

select_next_rot_image "${US0_ROT_A_ZIP}" "${US0_ROT_B_ZIP}"
if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

section "Rollback everything to previous version using only faux-mgs"

section "Update stage0next to previous image"

select_different_stage0 # fail or set INSTALL_IMAGE_ZIP and INSTALL_IMAGE_FWID
if update_stage0next "${INSTALL_IMAGE_ZIP}"
then
    success "stage0next updated: reading==goal (${INSTALL_FWID})"
else
    fatal "update stage0next failed"
fi
section "Reset RoT to measure and verify stage0next"
section "Persist stage0next to stage0"
section "reset to use the new stage0 image"
if ! persist_to_stage0_reset_and_test "${INSTALL_IMAGE_FWID}"
then
    fatal "failed to install $INSTALL_IMAGE_ZIP"
fi
#------
section "Update first RoT Hubris master image"

select_next_rot_image "${US0_ROT_A_ZIP}" "${US0_ROT_B_ZIP}"
if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

section "Update second RoT Hubris master image"

select_next_rot_image "${US0_ROT_A_ZIP}" "${US0_ROT_B_ZIP}"
if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

section "Update SP Hubris image to master"
