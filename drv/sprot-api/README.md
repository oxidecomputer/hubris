# Service Processor to Root of Trust Communications Over SPI

## Overview

The communications between the Service Processor (SP) and the
Root of Trust (RoT) over SPI is described.

In the Gimlet compute sled, and designs sharing Gimlet's SP/RoT implementation,
the LPC55 RoT has three means of communication only one of which is available
after boot is complete:

  - A Single Wire Debug (SWD) interface
  - A serial port for personalization during manufacturing.
  - A SPI link to the SP for production use.

    Note that an SWD interface from the RoT to the SP is used to
    securely bring-up the SP.  It can be used to establish shared data and
    retrieve SP measurements. Is not available beyond early SP boot.

The use and security of the SWD and serial interfaces is out of scope for this
document.

## Requirements

The message transport over SPI shall have:

  - Forward compatibility
      - Versioned protocol
      - Discoverable communications parameters
      - Encapsulation of higher layer communications
  - Resilience to data corruption due to:
      - bit errors,
      - FIFO underflow or overflow
      - loss of synchronized SP/RoT communication state
  - Support for re-establishment of trust
      - Simple identification of the RoT firmware (pre-attestation)
      - Updating firmware


### Forward Compatibility

#### Motivation

It should be assumed that units shipped from a factory will have some
initial working version of firmware and configuration installed. However,
it may be that a system shipped is not be put into service for a
significant amount of time. Such a system may have missed many important
updates, some which may knowingly or unknowingly break various management
interfaces with respect to an old implementation.

There is a conflict between maintaining backward compatibility with
everything previously released and shipping what is thought to be the
best implementation.  The burden of backwards compatibility amplifies
the test matrix exponentially, bloats code, and makes maintenance
increasingly difficult. If one is able to enforce version N across the
entire rack, or N and N-1 during transition, that burden is minimized.

The ability to discard or fix other interfaces remains if low-level update
interfaces are maintained. Low-level update interfaces can be changed,
but may require maintaining multiple implementations for each incompatible
version or upgrading through a strict series of releases in order to
arrive at a currently supported release. To be avoided are cases where
field service or RMA service is required to bring a unit into conformance.

One would also like to be able to re-key or otherwise update
security-related mechanisms and policies.  Such capabilities would be
enabled by the firmware that is installed and are outside the scope of
this document.

From experience with large-fleet root of trust updates, it is important
to have some unsecured information available when things go wrong.

If higher-level messages carried by the SP to RoT communications are
allowed to change, then the base-level should still be able to provide
information needed to determine what updates are necessary.  Namely,
any version or communications parameters needed to perform an update.
One still needs to use proper attestation to verify this unsecured
information.  But in cases where, perhaps, the right keys aren't
installed, or an installation has been botched but an automated update
is still possible, there is some information to get started with.

  1)  Can report basic facts needed to test and drive towards compliance, e.g.:
      1)  LPC55 version information (part number, ROM CRC)
      2)  Current running version.
      3)  Serial number or other unique instance identifier.
      4)  Communications parameters (buffer sizes, max CLK speed, FIFO depth)
      5)  Current boot policy (e.g. A/B image selection, FW update state machine state)
      6)  Key ID(s) of trusted keys.
      7)  Key ID(s) of own keys.
  2)  Reliably deliver a properly signed firmware blob for installation.
  3)  Can transport alternate protocols if they become available.
 
Some of these items may be sufficiently supported by other stable APIs,
such as those offered by the Update Server, Sprockets and DICE.
Those APIs, or portions of APIs that are to be considered part of the stable
update capability must be identified as such.

## Protocol

### Physical Layer SPI Protocol

The SP implements a basic SPI controller (master) and the RoT a SPI
target (slave). Master Out Slave In (MOSI) and Master In Slave Out
(MISO) are sampled on the rising edge of the clock signal (SCLK) when
Chip Select is asserted (CSn is active low). The RoT indicates
that it has a response ready by asserting `ROT_IRQ` (also active low), and
deasserts at the end of the SPI frame (CSn deasserted).

#### Performance

The SPI bus has no flow control and the SP has control of the data rate used.

In the case of the NXP LPC55, performance may be improved over the
initial programmed I/O implementation by using a wider FIFO configuration
(16-bits instead of 8), or by using DMA.

The SP is currently required to use the slowest clock possible (about 800kHz)
for the LPC55 to keep up with the data flow.
That rate can be doubled if the LPC55 is running at 96Mhz instead of 48Mhz.
Using DMA on the RoT should allow much faster clocking.

Note that if the programmed IO implementation is the one that first goes
to production, then the SP should use the slow clock and StatusReq to
query the RoT implementation and only configure a faster clock if the
RoT is able.

Alternatively, during the RoT's SWD bringup of the SP, SPI clock
configuration could be deposited in the SP RAM for use during SP initialization.

#### Reliability

SPI is not a flow-controlled interface, therefore any SPI I/O is
susceptible to underflow or overflow issues and can be aggravated by
changes in overall firmware behavior.  These problems may not become
evident until firmware is more widely deployed.

Use of DMA can result in a better performing and therefore more reliable
implementation.

Though the SP to RoT connection is not seen as a risk for signal
integrity issues, one still needs to consider the possibility of transient
communication errors.

While physical attacks on the SP to RoT SPI connection are considered
out of scope, measures to improve data reliability can make an attacker's
job more challenging.

The RoT, as the target device, does not control the SPI clock. It does
detect flow errors, i.e. receive overrun and transmit underrun.

The SP is able to set the pace and does not suffer flow errors and also
does not have direct evidence of RoT flow errors.

#### Data Integrity

The SPI physical layer does not address data integrity.

Introducing a CRC to the message makes detection on either side of the
interface much easier.

The `ErrorRsp` message informs the SP that the RoT did not successfully
transmit or receive the previous message.  Error response codes include
flow and CRC errors which are transient. The SP can retransmit to recover
from these errors.

As long as the likelihood of a flow error is sufficiently low,
overall performance of the SPI interface remains acceptable, even
with retries. Only one retry has been required for any failed transfer
during testing.

### Message Structure

A message consists of an four byte header, a variable payload, and a trailing 16-bit CRC.
The total message length cannot exceed the length specified in the sprot-api crate.

    The first 8-bytes of a message received by the LPC55 are special
    in that they will fit completely into the receive FIFO and are
    therefore much less susesptible to software induced overrun errors
    manifesting as dropped bytes.

    If the LPC55 FIFOs are configured for 16-bit widths, then 16 byte
    message can be accomodated before lack of software service would
    result in a flow error.

The message header is encoded in four bytes:

  - A Protocol identifier (`VERSION_1 = 0x01`),
  - enum MsgType as a `u8` describing the content of the message.
  - Payload length `U16<byteorder::LittleEndian>` length (LSB, MSB)

The payload must accommodate at least one 512-byte block plus overhead
for the Update Server's flash write operation.

A trailing 2-byte CRC16 follows the payload. It is computed over the
header and payload without the CRC16 itself.

Initial supported protocols are:

 - 0x00, the null protocol. Any subsequent byte is ignored by the receiver.
 - 0x01, the protocol documented here (`VERSION_1`)
 - 0xb2, indicates that the RoT is not currently servicing its FIFOs.
   (0xb2 = BZ = Busy)
 - 0xff, reserved for internal use to denote an unsupported protocol
 - All others are reserved for future implementations.

The payload length is a u16 and may be zero bytes in cases where
the MsgType is enough, e.g. a status request.

Error conditions include indication that messages exceed the receiver's
buffering limits, there is a flow error, or where insufficient data
is received.

The MsgType allows for multiplexing the communications link.
While Sprockets is intended to be the primary user, MsgType allows
for satisfying the previously stated requirements for non-Sprockets
messages and for accommodating future work without invalidating the
needs for day-one interface stability.

The RoT exchanges data with the SP for as long as `CSn` is asserted and
the SP is clocking data. If the SP clocks more data in or out than the
RoT's buffers allow, the RoT will discard or zero-fill FIFO data until
the end of frame.

Note that it is allowed that the message being sent from the SP to the RoT
has a length different than the message being sent from the RoT to the SP.
The validity of any received message is based on message validation and,
in the case of the RoT, lack of any flow errors.

When the RoT has no response for a previously received SP message,
it will have queued up a buffer of zero bytes before waiting for start of frame.

When interpreting the received data on either the SP or RoT, a message
always begins on the first byte. Any payload bytes that extend beyond the
payload length as specified in the header should be zeros and are ignored.

    Note: some SPI implementations use leading dummy bytes as a
    means to meet realtime requirements for responding to the same
    message as currently being received. The RoT makes no attempt to
    respond to a message from the SP within the same SPI frame.

### Message Types

See drv/sprot-api/src/lib.rs for the full list of messages.
Message types include:

  - _Invalid_: 0x00 is reserved and invalid as a message type. It is ignored.
  - _ErrorRsp_: 0x01 Errors are reported via a one byte payload containing
            a `MsgError` documented in `drv/sprot-api`.
  - _EchoReq/EchoRsp_ - 0x02/0x03 A simple echo message type for testing communications between SP and RoT.
    The payload returned is a copy of the payload received by the RoT.
    The CRC will differ because of the change in message type from EchoReq to
    EchoRsp.
  - _StatusReq/StatusRsp_ - 0x04/0x05 The RoT can report basic information about itself for use in cases where trust has not yet been established.
    TODO: *The format of the StatusRsp message is not final and needs discussion.*
  - _SprocketsReq/SprocketsRsp_ - 0x06/0x07 The payload represents a Sprockets message.
  - _SinkReq/SinkRsp_ - 0x08/0x09 The SP generates and sends repeated messages
    to test the RoT's ability to exchange messages without error.
    The `SinkRsp` is just a header, no payload.
    On error, an `ErrorRsp` message is sent.

### SP Timeouts

The SP needs to be able to timeout on waiting for a response from RoT.
The RoT has no general "ready" or "busy" indication,
it only has `ROT_IRQ` to indicate that a non-null response is ready.

The timeout could be accomplished by an SP-wide watchdog or a more localized
mechanism. The implications of particular failures or lack of synchronization
needs to be understood to develop an appropriate policy.

    TODO: SP timeout is not yet tuned. It needs review.

## Testing

### Test cases

#### Pulse (Assert then Deassert) Chip Select (CSn)

Pulsing CSn (asserting and de-asserting the chip select signal) is meant
to clear `ROT_IRQ` and flush RoT's Tx buffer.

If the RoT is busy processing the previous SP request message, then
the pulse may be ignored or delayed until processing of that previous
message is complete.

If the pulse happens while Rx is idle the (possibly all-zeros) message that the RoT
has queued will be discarded and an all-zeros message will then be queued.

CSn pulse duration is 10ms, but this may be adjusted later.

  1 Test with SP and RoT idle, fetch message from RoT and expect all zero bytes.
  2 Use lower-level SPI driver to send an `EchoReq`. Do not send pulse, Use lower level SPI driver to fetch RoT message, expect an `EchoRsp` or `ErrorRsp` if a correct CRC32 was not included in the header.
  3 Use lower-level SPI driver to send an `EchoReq`.
    See `ROT_IRQ` asserted.
    Send pulse.
    Use lower level SPI driver to fetch RoT message, expect all zeros.

#### RoT not ready to receive

The RoT has no explicit ready state visible to the SP other than the SP trying to send a message and getting a response.

The SP sending a CSn pulse, or using a timeout while sending a message seem to cover all the cases.

The RoT queus up a `BUSY` code (0xB2) when not attending its FIFOs.

### Running tests

The implementation has been tested on Gimlet, Gemini, and Gimletlet
Sidecar and PSC also require testing.

#### Test EchoReq and EchoRsp:

```sh
  $ SPI=3 # if Gimletlet
  $ SPI=4 # if Gemini
  $ APP=<path to app.toml>
  $ # Send an EchoReq with a 1-byte payload of 0x09 and corresponding CRC16
  $ # of 0xE120
  $ cargo xtask humility $APP -- spi -p $SPI -w 1,2,1,0,9,32,225

  ...
  [Ok([])]
  $ # Until the next command is executed, `ROT_IRQ` will be asserted.
  $ cargo xtask humility $APP -- spi -p 3 -r -n 7
  humility: attached to 0483:374f:0046001F3039510834393838 via ST-Link V3
  humility: SPI master is spi3_driver
               \/  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
  0x00000000 | 01 03 01 00 09 94 97

```

#### Test EchoReq and pulse CSn to clear RoT Tx and `ROT_IRQ`:

Same as above, but pulse CSn to clear the RoT Tx buffer.

```sh
$ cargo xtask humility $APP -- spi -p $SPI -w 1,2,1,0,9,32,225
...
[Ok([])]

$ humility hiffy -c SpRot.pulse_cs -a delay=10

# Note that the `humility sprot --pulse $DELAY` command returns the state of
`ROT_IRQ` before and after the pulse (asserted=1).

$ cargo xtask humility $APP -- spi -p $SPI -r -n7
..
humility: SPI master is spi3_driver
             \/  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
0x00000000 | 00 00 00 00 00 00 00                            | ......
$
```

#### Test sinking various count and size buffers from SP to RoT

See test runs

The ErrorReq/ErrorRsp and SinkReq/SinkRsp messages are not expected to be useful in production and should be conditionally compiled or removed.

#### Test StatusReq and StatusRsp:

See test runs

### Gimletlet Test Points

+------+------+------+------+
|      | MISO | RSTn | IRQn |
+------+------+------+------+
| GND  | SCK  | MOSI | CSn  |
+------+------+------+------+

### Test runs

Retrieve the status structure from the RoT.
```sh
$ humility hiffy -c SpRot.status
SpRot.status() => Status {
    supported: 0x2,
    bootrom_crc32: 0x47ae8b8d,
    epoch: 0x0,
    version: 0x0,
    buffer_size: 0x446,
    rx_received: 0x1,
    rx_overrun: 0x0,
    tx_underrun: 0x0,
    rx_invalid: 0x0,
    tx_incomplete: 0x0
}
```

Pulse the chip select signal to clear the RoT Tx buffer.
One can send a bad message first using direct SPI hiffy calls to elicit an error
response.
```sh
$ humility hiffy -c SpRot.pulse_cs -a delay=10
SpRot.pulse_cs() => PulseStatus {
    rot_irq_begin: 0x0,
    rot_irq_end: 0x0
}
```

If the sink feature is enabled, then the SP can be asked to generate N messages
of M size to send to the RoT, simulating a firmware image delivery.
A large transfer like this will usually require some retries for individual messages.
```sh
$ humility hiffy -c SpRot.rot_sink -a count=300 -a size=512
SpRot.rot_sink() => SinkStatus { sent: 0x12c }
```

Test the Update API over SpRot.
Retrieve the LPC55 flash block size.
```sh
$ humility hiffy -c SpRot.block_size
SpRot.block_size() => 0x200
```

Update API: set update destination
```sh
$ humility hiffy -c SpRot.prep_image_update -a image_type=ImageB
SpRot.prep_image_update() => ()
```

Update API: abort update
```sh
$ humility hiffy -c SpRot.abort_update
SpRot.abort_update() => ()
```

Update API: start another update
```sh
$ humility hiffy -c SpRot.prep_image_update -a image_type=ImageB
SpRot.prep_image_update() => ()
```

Update API: write a block of zeros
```sh
$ humility hiffy -c SpRot.write_one_block -a block_num=0 -i <(dd if=/dev/zero bs=512 count=1)
SpRot.write_one_block() => ()
```

Update API: finish the update
```sh
$ humility hiffy -c SpRot.finish_image_update
SpRot.finish_image_update() => ()
```

Retrieve final status. Note the 17 overruns that occurred during this successful run.
```sh
$ humility hiffy -c SpRot.status
SpRot.status() => Status {
    supported: 0x2,
    bootrom_crc32: 0x47ae8b8d,
    epoch: 0x0,
    version: 0x0,
    buffer_size: 0x446,
    rx_received: 0x134,
    rx_overrun: 0x11,
    tx_underrun: 0x0,
    rx_invalid: 0x0,
    tx_incomplete: 0x0
}
```

Update API, retrieve current version (deprecated)
This information is redundant with information in the Status structure.
```sh
$ humility hiffy -c SpRot.current_version
SpRot.current_version() => ImageVersion {
    epoch: 0x0,
    version: 0x0
}

```

## TODO/Issues

  - Agree on identification information needed at earliest, unsecured boot for manufacturing and production workflows.
  - Complete testing is needed to ensure that the interface has a high degree of stability and reliability.
  - This doc belongs in an RFD.
