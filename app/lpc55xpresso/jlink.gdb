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

stepi
