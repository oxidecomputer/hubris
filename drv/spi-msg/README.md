# Service Processor to Root of Trust Communications Over SPI

## Requirements

In the Gimlet compute sled, and designs sharing Gimlet's SP/RoT
implementation, the LPC55 root of trust has three means of communication
only one of which is available in production:

  - An SWD interface that can be used for development.
  - A serial port for personalization during manufacturing.
  - A SPI link to the SP for production use.

The use and security of the SWD and serial interfaces is out of scope for this
document.

SWD from RoT to SP is also used to securely bring-up the SP and can be used to establish shared data and retrieve SP measurements, but is not available beyond early SP boot.

It should be assumed that units shipped from a factory will have some
initial working version of firmware and configuration installed. However,
a system shipped may not be put into service for a significant amount
of time. An extreme example would be a unit set aside by a customer as
a spare that is not racked until several firmware releases have already
been deployed. The outdated firmware on such a spare needs to be brought
into compliance with the current rack release before it can participate
in a production workflow.

Oxide would like to retain the ability to have interface breaking releases
such as when some API or protocol has been found to be fundamentally
flawed.

Oxide would like to avoid having to support every old API and protocol
ever released in order to be able to bring units into compliance with
the active release.

Oxide would like to be able to re-key or otherwise update security-related
mechanisms and policies.

These goals are harder to meet at the lowest layers of the stack and
the SP/RoT communications protocol is a small part of that solution,
not the entire answer.

In order to support the larger goals above, the SP/RoT link's base level
requirements are that it:

  1)  Is reliable.
  2)  Can report basic facts needed to test and drive towards compliance:
      1)  LPC55 version information (part number, ROM CRC)
      2)  Current Oxide firmware version(s)
      3)  Serial number
      4)  Communications parameters (buffer sizes, max CLK speed, max burst size)
      5)  Current boot policy (e.g. A/B image selection, FW update state machine state)
      6)  Key ID(s) of trusted keys.
      7)  Key ID(s) of own keys.
  7)  Can support delivery of a properly signed firmware blob for installation.
  8)  Can transport alternate protocols if they become available.
 
Some of these items may be sufficiently supported by other means, such as
Sprockets. But when things go wrong with secured communications, this sort
of basic information is needed to support automated diagnosis and repairs.

## Protocol

### Physical Layer SPI Protocol

The SP implements a basic SPI controller (master) and the RoT a SPI
target (slave). Master Out Slave In (MOSI) and Master In Slave Out
(MISO) are sampled on the rising edge of the clock signal (SCLK) when
Chip Select[negated, active low] (CSn) is asserted. The RoT indicates
that it has a response ready be asserting ROT_IRQ [active low], and
deasserts when its transmit FIFO is empty.

It would be possible to shift to faster variants of SPI, e.g. using
rising and falling edges of SCLK to double the transfer rate. However, the
requirement for a stable and reliable interface means that any shift to an
alternate SPI protocol should be negotiated after initial communications
to ensure that the oldest issued firmware can always be updated.

### Message Structure

A message consists of a four byte header and a variable payload which
is not to exceed the length specified in the spi-msg crate.

The four bytes are the protocol identifier, the least significant byte
of the payload length, the most significant byte of the payload length,
and the message type (MsgType).

The Status message, described later, should publish the payload limit
currently in effect so that future implementations have the option to
extend it.

Initial supported protocols are:

 - 0x00, the null protocol. Any subsequent byte is ignored by the receiver.
 - 0x01, the protocol documented here
 - 0xff, reserved for internal use to denote an unsupported protocol
 - All others are reserved for future designs.

The payload length is a u16 and may be zero bytes in cases where
the MsgType carries sufficient information. Error conditions are
recognized where messages exceed the receiver's buffering limits or
where insufficient data is received.

The MsgType is used to allow for multiplexing the communications
link. While Sprockets is intended to be the primary user, MsgType allows
for satisfying the previously stated requirements for non-Sprockets
messages and for accommodating future work without invalidating the
needs for day-one interface stability.

A message from the SP to RoT or RoT to SP begins when the first byte
clocked out after CSn is asserted is 0x01 and ends when the 0x01
protocol's payload length has been satisfied.

### Message Types

Message types include:

  - _Invalid_: 0x00 is reserved and invalid as a message type.
  - _ErrorRsp_: Protocol errors are reported with an optional payload.
	  + No payload format has been determined.
	  + There may be a version mismatch between the SP and the RoT if the message type was introduced after the first production release.
	  + There may have been a transient communications error.
  - _EchoReq/EchoRsp_ - A simple echo/ping message type for testing communications between SP and RoT.
  - _StatusReq/StatusRsp_ - The RoT can report basic information about itself for use in cases where trust has not yet been established.
  - _SprocketsReq_ - The payload represents a Sprockets message.
  - _Unknown_ - An reserved, internal representation for an unsupported message type.

## Testing

The implementation has been tested on Gemini.

The RoT EchoReq and EchoRsp handling can be tested through the SP using
the existing humility spi command:
```sh
  $ # Test EchoReq and EchoRsp:
  $ cargo xtask humility app/gemini-bu/app.toml -- spi -p 4 -w 1,4,0,2,4,5,6,7
  ...
  scratch size = 512
  humility: SPI master is spi4_driver
  [Ok([])]

  $ cargo xtask humility app/gemini-bu/app.toml -- spi -p 4 -r -n 8
  scratch size = 512
  humility: SPI master is spi4_driver
               \/  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
  0x00000000 | 01 04 00 03 04 05 06 07                         | ........


  $ # Test StatusReq and StatusRsp:
  $ cargo xtask humility app/gemini-bu/app.toml -- spi -p 4 -w 1,0,0,4
  ...
  scratch size = 512
  humility: SPI master is spi4_driver
  [Ok([])]

  $ cargo xtask humility app/gemini-bu/app.toml -- spi -p 4 -r -n 8
  ...
  humility: SPI master is spi4_driver
               \/  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
  0x00000000 | 01 04 00 05 8d 8b ae 47                         | .......G

```
The last four bytes of the response is the CRC32 of the LPC55 ROM.

To test sprockets messages from a laptop RS232 port to Gemini's SP to
the RoT and back:

Connect a USB to RS232 adaptor, such as the "[OIKWAN USB Serial Adapter with FTDI Chipset](https://www.amazon.com/gp/product/B0759HSLP1)"

A "[9 Pin Serial Male to 10 Pin Motherboard Header Panel Mount Cable](https://www.amazon.com/StarTech-com-Serial-Motherboard-Header-Panel/dp/B0067DB6RU)" makes simple work of the connection. Alternatively, use dupont wires:

Use dupont female to female wires to connect pins:
  - `DB9:2(RXD)` to `J302:3(RS232_TX)`
  - `DB9:3(TXD)` to `J302:5(RS232_RX)`
  - `DB9:5(GND)` to `J302:9(GND)`

DB9, looking at the pins:
```
 ___________
( 1 2 3 4 5 )
 \ 6 7 8 9 /
  \_______/
```

Gemini connector J302 labeled:

```
        "SERIAL TO HOST"
    (TODO NOTE SIGNAL LEVELS)
       +-----------------+
J302   |  2  4  6  8 10  |
       |  1  3  5  7  9  |
       +-------   -------+
```

Identify the device path for your USB to RS232 adaptor. On the author's Linux laptop, it shows up as /dev/ttyUSB0.

```sh
SERIAL=/dev/ttyUSB0
```

In a [sprockets workspace](https://github.com/oxidecomputer/sprockets):
```
  cargo run -- -b 115200 -p $SERIAL get-certificates
  cargo run -- -b 115200 -p $SERIAL get-measurements

## TODO/Issues

  - A frame check sequence is needed for the receiver to recognize underruns on the part of the transmitter.
  - A retry scheme may be useful to improve reliability.
  - The SP may need to make a trade-off between SPI clock speed and reliability.
	  + It may be useful to do retries at slower speeds.
	  + Logging of retries, clock speed changes, and other information related to reliability is needed to make any improvements and to identify problems early.
  - Ensure that bytes transmitted beyond payload length are properly ignored/processed (log unexpected conditions, do not impede driving towards compliance).
  - Agree on identification information needed at earliest, unsecured boot for manufacturing and production workflows.
  - Agree on RoT firmware update workflows.
  - If more a more detailed MsgType::Error payload is useful, work that out early.
  - Complete testing is needed to ensure that the interface has a high degree of stability and reliability.
  - This doc belongs in an RFD.
