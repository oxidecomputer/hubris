target extended-remote :3333

# print demangled symbols
set print asm-demangle on

# set backtrace limit to not have infinite backtrace loops
set backtrace limit 32

# detect unhandled exceptions, hard faults and panics
break HardFault

monitor tpiu config internal itm.txt uart off 16000000

# enable ITM ports
monitor itm port 0 on
monitor itm port 1 on
monitor itm port 8 on

load

continue
