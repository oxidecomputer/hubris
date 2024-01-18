#!/bin/bash


# An end-to-end test script to prove that we can:
#   - start with RoT and SP on Hubris master branch images,
#   - update Hubris on both to new branch,
#   - update bootleby
#   - rollback bootleby
#   - rollback hubris to master branch.
#
# Humility is used to reset to initial conditions and to verify successful
# update.
# Otherwise, faux-mgs is used for everything else to mimic the control plane.
#
# Note: Build all of the needed products in the master branch and branch under
# test before running this script. Testing on Emeryville test units requires
# signing images with Staging Development keys.

# Note that most of the functions in this script are executed in the primary
# shell and as such affect global state. This is a cheap and sloppy way
# to return values used later.
#
# These scripts should be considered as requirements documents for an
# automated end-to-end test written in rust that we will use to qualify
# any Hubris or # Bootleby build for production release.
#
# TODO: Rewrite this script as a rust program.

set -e
set -u
export RUST_BACKTRACE=1

PROG=$(basename "$0")
PROG_DIR="$(dirname "$(readlink -f "$0")")"
PATH="${PROG_DIR}:${PATH}"
if which toilet 2>/dev/null
then
	HAS_TOILET=true
else
	HAS_TOILET=false
fi

usage() {
  ec="${1:?Missing exit code}"
  shift
  msg="${*:-}"
  shift
  if (( ec != 0 ))
  then
    exec 1>&2
  fi
  [[ -n "$msg" ]] && echo "$msg"
  echo "Usage:"
  echo "$PROG [-p pseudo-tty-path] [-h]"
  echo "  -p pts # Send flashy section text to pts"
  echo '  -h # this message'
  exit "${ec}"
}

PTS=""
while getopts "hp:" opt; do
	case $opt in
		p) PTS="${OPTARG}";;
		h) usage 0;;
		?) usage 1 "Invalid option";;
	esac
done
shift $((OPTIND-1))

source hubris-util.sh
# set +e

case $(uname -s) in
  SunOS) PFEXEC="pfexec";;
  *) PFEXEC="";;
esac

if [[ -n "${PTS}" ]]
then
  WIDTH="$(stty -a < "${PTS}" | awk '-F;' '/columns/ {n=split($3, A, /\s+/); printf "%s", A[3]}' -)"
else
  WIDTH="$(stty -a | awk '-F;' '/columns/ {n=split($3, A, /\s+/); printf "%s", A[3]}' -)"
fi

pts() {
  if [[ "${1:-}" = color ]]
  then
      shift
      if $HAS_TOILET
      then
	      cmd="toilet -F gay --width ${WIDTH} --font small $*"
      else
	      cmd=": $*"
      fi
    else
      if $HAS_TOILET
      then
        cmd="toilet --width ${WIDTH} --font mini $*"
      else
        cmd=": $*"
      fi
  fi
  if [[ -n "${PTS}" ]]
  then
	date +%H:%M:%S > "${PTS}"
	$cmd > "${PTS}"
  fi
  date +%H:%M:%S
  $cmd
}

CONFIG="${1:-config-$(uname -n).sh}"
if [[ ! -r "${CONFIG}" ]]
then
  fatal "Cannot read ${CONFIG}. Specify alternate config file as first argument."
fi

source "${CONFIG}"

check_dependencies() {
    # shellcheck disable=SC2153
    check_readable "${ALL_IMAGES[@]}"

    DEPEND=( )
    DEPEND+=( "${ROT_FWID}" )
    DEPEND+=( "${FAUX_MGS}" )
    DEPEND+=( jq )
    DEPEND+=( unzip )
    DEPEND+=( stat )
    DEPEND+=( hubedit )
    DEPEND+=( zextract )
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
pts Check dependencies
check_dependencies

section Show the FWID and GITC values from each image
for image in "${ALL_IMAGES[@]}"
do
    fwid="$(fwid_from_zip "${image}")"
    gitc="$(image_gitc "${image}" 2>/dev/null)"
    if [[ -z "${gitc}" ]]
    then
	    fact "$(printf "Image: %s\n\tFWID: %s\n\tGITC: \x1b[43mNone" "${image}" "${fwid}")"
    else
	    fact "$(printf "Image: %s\n\tFWID: %s\n\tGITC: %s\n" "${image}" "${fwid}" "${gitc}")"
    fi
done

# We don't want to update the SP or RoT when a system is powered up
POWER_STATE="$(power_state)"
if [[ "${POWER_STATE}" != "A2" ]]
then
    fatal "Device must be in power_state A2, not '${POWER_STATE}'"
fi

# No inadvertent humility parameters through env
unset HUMILITY_ARCHIVE
unset HUMILITY_ENVIRONMENT
unset HUMILITY_PROBE

# Using Humility to flash an RoT image is not the same as installing with
# MGS/lpc55-update-server. The padding on the last flash page of the image
# and any trailing programmed pages are not guarenteed to be in the same
# state using the two methods.
# This can result in different FWIDs for the same executable which makes
# it hard to judge success by comparing FWIDs.
# Devices that still have images from the manufacturing process are not likely
# to match the desired FWIDs.
# For that reason, the initialization sequence isn't finished until only
# MGS/lpc55-update-server installed images are present on the RoT.
initialize_test() {
    section Use humility to install master branch images
    pts "Init SP & RoT using Humility"
    action "Flash SP with master branch image"
    action "${HUMILITY} --archive ${MASTER_SP_ZIP} -p ${SP_PROBE} flash"
    # shellcheck disable=SC2086
    ${PFEXEC} ${HUMILITY} --archive "${MASTER_SP_ZIP}" -p "${SP_PROBE}" flash 2>&1 |
        grep -q 'already flashed' && echo Already Flashed SP

    # shellcheck disable=SC2086
    ${PFEXEC} ${HUMILITY} -p "${SP_PROBE}" reset
    sleep 3

    # action "Erase RoT flash bank A"
    # rot_bankerase a all
    action "Flash RoT A with master branch image using Humility"
    action "${HUMILITY} --archive ${MASTER_ROT_A_ZIP} -p ${ROT_PROBE} flash"
    # shellcheck disable=SC2086
    ${PFEXEC} ${HUMILITY} --archive "${MASTER_ROT_A_ZIP}" -p "${ROT_PROBE}" flash 2>&1 |
        grep -q 'already flashed' && echo Already Flashed ROT A
    sleep 3 # XXX get rid of the timeout

    # action "Erase RoT flash bank B"
    # rot_bankerase b all
    action "Flash RoT B with master branch image using Humility"
    action "${HUMILITY} --archive ${MASTER_ROT_B_ZIP} -p ${ROT_PROBE} flash"
    # shellcheck disable=SC2086
    ${PFEXEC} ${HUMILITY} --archive "${MASTER_ROT_B_ZIP}" -p "${ROT_PROBE}" flash 2>&1 |
       grep -q 'already flashed' && echo Already Flashed ROT B

    # action "Erase Stage0Next"
    # rot_bankerase stage0next all

    action "Reset RoT to ensure that one of the master branch images is active"
    # shellcheck disable=SC2086
    ${PFEXEC} ${HUMILITY} -p "${ROT_PROBE}" reset
    poll_rot_ready

    action "Use faux-mgs to update the RoT alternate image to Hubris master"
    select_next_rot_image "${MASTER_ROT_A_ZIP}" "${MASTER_ROT_B_ZIP}"
    if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
    then
        success RoT Hubris image has been updated.
    else
        error RoT Hubris image failed to updated.
        fatal "Failed to activate new RoT image."
    fi

    action "Use faux-mgs to update the RoT alternate image to Hubris master"
    select_next_rot_image "${MASTER_ROT_A_ZIP}" "${MASTER_ROT_B_ZIP}"
    if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
    then
        success RoT Hubris image has been updated.
    else
        error RoT Hubris image failed to updated.
        fatal "Failed to activate new RoT image."
    fi

    action "Persist RoT image A using faux-mgs"
    action "fm component-active-slot -s 0 -p rot"
    OK="$(fm component-active-slot -s 0 -p rot | jq -r -c ".${INTERFACE} | keys[0]")"
    [[ "${OK}" == "Ok" ]] || fatal "Failed to set persistent Hubris image."
    action "Reset RoT"
    reset_rot_and_poll_ready

    ACTIVE=$(get_active_rot_bank)
    case "${ACTIVE}" in
        A) success "Master branch images are running with RoT booting from bank A.";;
        *) error "Rot active image is ${ACTIVE}"
            fatal "Rot active image is not A"
            ;;
    esac
}

initialize_test

if is_rot_boot_info_supported_by_sp
then
    fatal "Master branch supports new API, this test is out of date."
fi

# The SP and RoT under test now have Hubris master branch images that do not
# support stage0 update and do not expose the FWID hashes unless there
# is a valid image for the respective Hubris image bank.
#

# "US0" is used as a prefix for the "Update Stage0" branch.

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

fact "SP image hubris_archive_id: $(sp_v2_archive_id)"

section "Use faux-mgs to update SP with update-stage0 branch image"
pts "New Image to SP"

if ! update_sp "${MASTER_SP_ZIP}" "${US0_SP_ZIP}"
then
    fatal "Was not able to update SP to ${US0_SP_ZIP}"
fi

if is_rot_boot_info_supported_by_sp
then
    success SP supports new API
else
    fatal "SP does not support new API"
fi


section "Use faux-mgs to update Rot with update-stage0 branch image"
pts "New Image to RoT bank ${ROT_UPDATE_BANK}"
if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

if is_rot_boot_info_supported_by_rot
then
    pts New messages are supported
    success RoT supports new API
else
    fatal "RoT does not support new API SP_RBI_SUPPORT=${SP_RBI_SUPPORT} ROT_RBI_SUPPORT=${ROT_RBI_SUPPORT}"
fi

section Check image signatures against installed RoT CMPA/CFPA.
pts "Verify all sigs vs RoT"

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
pts "Select next Bootleby"
get_rot_state
# These vars are now set/refreshed from V3 state:
#  ACTIVE, PENDING_PERSISTENT_BOOT_PREFERENCE, PERSISTENT_BOOT_PREFERENCE,
#  TRANSIENT_BOOT_PREFERENCE, SLOT_A_FWID, SLOT_B_FWID,
#  STAGE0_FWID, STAGE0NEXT_FWID, SLOT_A_STATUS_EPOCH,
#  SLOT_A_STATUS_VERSION, SLOT_A_STATUS_ERR, SLOT_B_STATUS_EPOCH,
#  SLOT_B_STATUS_VERSION, SLOT_B_STATUS_ERR, STAGE0_STATUS_EPOCH,
#  STAGE0_STATUS_VERSION, STAGE0_STATUS_ERR, STAGE0NEXT_STATUS_EPOCH,
#  STAGE0NEXT_STATUS_VERSION, STAGE0NEXT_STATUS_ERR
fact "Installed stage0 fwid=${STAGE0_FWID}"
fact "Installed stage0next fwid=${STAGE0NEXT_FWID}"

section "Show that stage0 can be updated by installing a different image"

select_different_stage0() {
    BEGIN_STAGE0_FWID="${STAGE0_FWID}"
    BEGIN_STAGE0NEXT_FWID="${STAGE0NEXT_FWID}"
    _CONTENTS=( "${BEGIN_STAGE0_FWID}" "${BEGIN_STAGE0NEXT_FWID}" )
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

BSF="$(echo "${BEGIN_STAGE0_FWID}" | sed -e 's=^\(....\).*\(....\)$=\1..\2=')"
IIF="$(echo "${INSTALL_IMAGE_FWID}" | sed -e 's=^\(....\).*\(....\)$=\1..\2=')"
pts "Try Stage0 from $BSF to $IIF"

section Different Bootleby to Stage0Next
pts "Update Stage0Next"
if update_stage0next "${INSTALL_IMAGE_ZIP}"
then
    success "stage0next updated: reading==goal (${INSTALL_FWID})"
else
    fatal "update stage0next failed"
fi

section Different Bootleby to Stage0Next
pts "Persist Stage0Next"
if persist_to_stage0_reset_and_test "${INSTALL_IMAGE_FWID}"
then
    pts color "Stage0 Success"
else
    pts "Fail"
    fatal "failed to install $INSTALL_IMAGE_ZIP"
fi

section "Update alternate RoT Hubris image to complete the transition to the new image"

select_next_rot_image "${US0_ROT_A_ZIP}" "${US0_ROT_B_ZIP}"
pts "New Image to RoT Bank ${ROT_UPDATE_BANK}"
if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

section "Rollback everything to previous version using only faux-mgs"
pts "Perform rollback"

section "Update stage0next to previous image"
pts "Rollback: Bootleby"

select_different_stage0 # fail or set INSTALL_IMAGE_ZIP and INSTALL_IMAGE_FWID
BSF="$(echo "${BEGIN_STAGE0_FWID}" | sed -e 's=^\(....\).*\(....\)$=\1..\2=')"
IIF="$(echo "${INSTALL_IMAGE_FWID}" | sed -e 's=^\(....\).*\(....\)$=\1..\2=')"
pts "Try Stage0 from $BSF to $IIF"
if update_stage0next "${INSTALL_IMAGE_ZIP}"
then
    success "stage0next updated: reading==goal (${INSTALL_FWID})"
else
    fatal "update stage0next failed"
fi
section "Reset RoT to measure and verify stage0next"
section "Persist stage0next to stage0"
section "reset to use the new stage0 image"
if persist_to_stage0_reset_and_test "${INSTALL_IMAGE_FWID}"
then
    success Stage0 updated
    pts color Success
else
    fatal "failed to install $INSTALL_IMAGE_ZIP"
fi
#------
section "Update first RoT Hubris master image"

select_next_rot_image "${MASTER_ROT_A_ZIP}" "${MASTER_ROT_B_ZIP}"
if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

section "Update second RoT Hubris master image"

select_next_rot_image "${MASTER_ROT_A_ZIP}" "${MASTER_ROT_B_ZIP}"
if update_rot_hubris "${ROT_ZIP}" "${ROT_UPDATE_BANK}"
then
    success RoT Hubris image has been updated.
else
    error RoT Hubris image failed to updated.
    fatal "Failed to activate new RoT image."
fi

section "Rollback complete"
