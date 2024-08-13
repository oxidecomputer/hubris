# Exercise fixture for the STM32H7 FMC

This task maps in a 16-bit multiplexed PSRAM bus and lets you do incredibly
ill-advised things to it. It is intended for interface testing only, do not eat.


## Network protocol

This task can be bound to a UDP socket to allow ill-advised things over the
network. Here's the UDP protocol.

Packets sent to the device always begin with a two-byte header:
- `version` (1 byte): currently 0
- `command` (1 byte)

The only currently defined `command` is 0.

Following this is a series of arguments to command 0, which describe a sequence
of memory operations. Arguments are simply concatenated together. Each starts
with an identifier byte, and some are followed by additional bytes. Any sequence
of the following is acceptable:

- `address` (0) followed by a little-endian 32-bit address. This loads the
  address into the internal address register used by subsequent peeks and pokes.
- `peek` (1/2/3/4) reads an 8/16/32/64-bit (respectively) value from memory at
  the current address and concatenates it (in little endian order) onto the
  response packet.
- `peek_advance` (5/6/7/8) operate like the corresponding `peek` operations, but
  also advance the address register by the size of data being operated on.
- `poke` (9/10/11/12), followed by an 8/16/32/64-bit (respectively)
  little-endian value, writes that value into memory at the address in the
  address register. Nothing is appended to the response.
- `poke_advance` (13/14/15/16) operate like the corresponding `poke` operations,
  but also advance the address register by the size of data being operated on.

The response starts with a single byte, which is zero for success and non-zero
for failure. On success, the results of all peek operations are concatenated
after the first byte, in order.
