# Erase the CMPA area of 0A hardware
#
# The CMPA region of the LPC55 contains settings for secure boot.
# On 1B hardware, you can still use the built in ISP commands to erase
# memory and unset secure mode unless you have locked down a board (set the
# hash when programming or set the seal flag in the API).
#
# On 0A hardware these command do not seem to be available when any secure mode
# is set up. You can still access ISP mode and SWD but the regular read/write
# commands are restricted. Because we have access to SWD, it is still possible
# to unset secure mode by calling these functions manually. This is the
# rough equivalent to
#
# flash_config_t f;
#
# memzero(&f, sizeof(f));
# memzero(0x20004000, 0x1000);
#
# flash_init(&f);
# ffr_init(&f);
# ffr_cust_factory_page_write(&f, 0x20004000, 0);
#
# This is buggy because we're supposed to set the CPU frequency but there's
# no way to know that from the program. As long as the PLLs aren't running
# this should work well enough but it is recommended to run
#
# monitor read32 0x9e400 512
#
# afterwards to verify that the CMPA region has been completely erased. If
# it has not, running it again usually works.
#
# Q: Isn't this dangerous/buggy/hacky
# A: Absolutely. Ideally you would not use 0A hardware with secure booting
#    but this is designed to be here in the unfortunate even you need to work
#    on secure 0A hardware.

target extended-remote :3333

monitor halt

# Clear the space we'll be using for flash_config
monitor fill 0x20000000 0x1000 0x0

# set our stack to something that won't collide

set $sp = 0x20040000

# arg 0 = our flash_config
set $r0 = 0x20000000

# address of flash_init
set $pc = 0x1300409c
# end of flash_init
break *0x13004138

continue

# arg 0 = our flash_config
set $r0 = 0x20000000

# address of ffr_init
set $pc = 0x13004914
# end of ffr_init
break *0x13004930
continue

# Now clear our "CMPA" region
monitor fill 0x20004000 0x1000 0x0

# arg 0 = our flash_config
set $r0 = 0x20000000
# arg 1 = our CMPA region
set $r1 = 0x20004000
# arg 2 = seal (this must be 0!)
set $r2 = 0x0

# address of CMPA write function
set $pc = 0x13004c22
# end of CMPA write function
break *0x13004ca0
continue

