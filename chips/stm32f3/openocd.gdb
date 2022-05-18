target extended-remote :3340

# print demangled symbols
set print asm-demangle on

# set backtrace limit to not have infinite backtrace loops
set backtrace limit 32

# detect hard faults
break HardFault

monitor arm semihosting enable
