#!/usr/bin/env bash

# Load an image using the humility hiffy interface for the lpc55-update-server.
#
# Usage Example:
# ARCHIVE=hubris/target/rot-carrier/dist/a/build-rot-carrier.zip rot-update-tools/hiffy-rot-flash.sh rot-carrier-v2/b/final.bin ImageB
#
# In the above example, `ARCHIVE` is set to the current image running on the
# RoT. In this example, slot A is currently running, so we upload to slot b via
# a saved build in `rot-carrier-v2` and additionally give the keyword `ImageB`.

# NOTE: Make sure you are not runnning this on an ancient version of bash on Mac OSX

set -x

image=$1
image_type=$2

block_size=512
binsize=`ls -la $image | cut -w -f 5`
numblocks=$(("${binsize}" / "${block_size}"))
total_before_last_block=$((numblocks * "${block_size}"))
lastblocksize=$(("${binsize}" - "${total_before_last_block}"))
loopend=$(("${numblocks}"-1))

write_blocks() {
  start="${1:?Missing start block number}"
  shift
  end="${1:?Missing end block number}"
  shift
  block_image="${1:?Missing path to image}"
  shift

  echo "${start}"
  echo "${end}"

  for (( i=start; i<=end; i++))
  do
    if ! humility -a "${ARCHIVE}" hiffy -c Update.write_one_block -a block_num="${i}" -i <(dd if="${block_image}" bs="${block_size}" count=1 skip="${i}")
    then
      # Retry once
      #
      # The following errors have been observed
      #
      # [2022-12-01T17:07:49Z WARN  probe_rs::config::target] Using custom sequence for LPC55S69
      # humility: attached to 1fc9:0143:CW4N3IFK22UUX via CMSIS-DAP
      # humility hiffy failed: A core architecture specific error occured
      #
      # Caused by:
      #     0: Failed to read register DRW at address 0x0000000c
      #     1: An error specific to the selected architecture occured
      #     2: Target device responded with WAIT response to request.
      # Error: humility failed
      sleep 1
      if !  humility -a "${ARCHIVE}" hiffy -c Update.write_one_block -a block_num="${i}" -i <(dd if="${block_image}" bs="${block_size}" count=1 skip="${i}")
      then
        exit 1
      fi
    fi
  done
}

humility -a "${ARCHIVE}" hiffy -c Update.prep_image_update -a image_type="${image_type}"

# Begin by invalidating the header which resides at offset 0x130 (in block zero).
write_blocks 0 0 /dev/zero
write_blocks 1 "${loopend}" "${image}"
humility -a "${ARCHIVE}" hiffy -c Update.write_one_block -a block_num="${numblocks}" -i <(dd if="${image}" bs=1 count="${lastblocksize}" skip="${total_before_last_block}") 
# Lastly, write the correct block zero.
write_blocks 0 0 "${image}"

humility -a "${ARCHIVE}" hiffy -c Update.finish_image_update
