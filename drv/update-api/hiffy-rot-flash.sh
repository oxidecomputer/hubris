#!/bin/zsh

set -x

image=$1
image_type=$2

binsize=`ls -la $image | cut -w -f 5`
numblocks=$(($binsize / 512))
total_before_last_block=$((numblocks * 512))
lastblocksize=$(($binsize - $total_before_last_block))
loopend=$(($numblocks-1))

humility -a $ARCHIVE hiffy -c Update.prep_image_update -a image_type=$image_type

write_blocks() {
  start=$1
	end=$2
	block_image=$3
	
	echo $start
	echo $end

  for ((i=$start;i<=$end;i++))
	do
	   humility -a $ARCHIVE hiffy -c Update.write_one_block -a block_num=$i -i <(dd if=$block_image bs=512 count=1 skip=$i)
		 if [ "$?" -ne 0 ]
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
	      humility -a $ARCHIVE hiffy -c Update.write_one_block -a block_num=$i -i <(dd if=$block_image bs=512 count=1 skip=$i)
		 fi
		 if [ "$?" -ne 0 ]
		 then
		     exit 1
		 fi
	  
	done

}

write_blocks 1 $loopend $image
humility -a $ARCHIVE hiffy -c Update.write_one_block -a block_num=$numblocks -i <(dd if=$image bs=1 count=$lastblocksize skip=$total_before_last_block) 
write_blocks 0 0 /dev/zero
write_blocks 0 0 $image
humility -a $ARCHIVE hiffy -c Update.finish_image_update