target extended-remote :2331

# Display the pc instruction on break
display /i $pc

set print asm-demangle on

set backtrace limit 32

# detect hard faults
# break HardFault

# break SecureFault

load
monitor reset
monitor semihosting enable
# Send the monitor output to gdb
monitor semihosting IOClient 3

# The loading does not seem to initialize this properly
# Make sure these are 0x0 for now (double check this later)
mon reg r0 0x0
mon reg r1 0x0
mon reg r2 0x0
mon reg r3 0x0
mon reg r4 0x0
mon reg r5 0x0
mon reg r6 0x0
mon reg r7 0x0
mon reg r8 0x0
mon reg r9 0x0
mon reg r10 0x0
mon reg r11 0x0
mon reg r12 0x0

stepi
