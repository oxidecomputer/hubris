# Service Processor to Root of Trust Communications Over SPI

## Overview

The SPI based communications between the Service Processor (SP) and the
Root of Trust (RoT) is described.

In the Gimlet compute sled, and designs sharing Gimlet's SP/RoT implementation,
the LPC55 RoT has three means of communication only one of which is available
after boot is complete:

  - An SWD interface that can be used for development.
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
      - Tx FIFO underflow, and
      - Rx FIFO overflow
  - Ability to resynchronize SP/RoT communication state
  - Support for re-establishment of trust
      - Identification of the RoT firmware
      - Updating firmware


### Forward Compatibility

#### Motivation

It should be assumed that units shipped from a factory will have some
initial working version of firmware and configuration installed. However,
it may be that a system shipped is not be put into service for
a significant amount of time.

An extreme example would be a unit set aside by a customer as a spare that is
not racked until several firmware releases have already been deployed.
The outdated firmware on such a spare needs to be brought into compliance
with the current rack release before it can participate in production workflows.
The customer must be able to perform any updates with normal firmware update workflows.

Oxide would like to retain some ability to have interface breaking
releases without carrying an undue backwards compatibility burden.
For example, when some API or protocol has been found to be fundamentally
flawed, or other meaningful improvements necessitate abandoning old
message formats, it should only be necessary to maintain compatibility with
the original firmware update interface that enables RoT and SP updates
over a management network.  After that point, new update mechanisms
could be used and old ones could be retired.

Oxide would like to be able to re-key or otherwise update security-related
mechanisms and policies. This goal is largely orthogonal to SP/RoT
firmware update, and may be available via DICE at boot. But any scheme
that assumes proper keying before being operational may not be available
at the earliest stages of boot or when a system requires re-keying.

  Expect that Oxide will have a facility at some point that has sleds
  keyed for development, production, and RMA, if only to develop the
  associated workflows. A sled placed in the wrong rack should be easily
  identifiable so that it can be returned to its proper location without
  disturbing its non-volatile state.

Due to resource constraints, these goals are harder to meet at the lowest
layers of the stack and the SP/RoT communications protocol is a small
part of that solution, not the entire answer.

In order to support the larger goals above, the SP/RoT link's base level
requirements are that it:

  1)  Is reliable.
  2)  Can report basic facts needed to test and drive towards compliance, e.g.:
      1)  LPC55 version information (part number, ROM CRC)
      2)  Current Oxide firmware version(s)
      3)  Serial number or other unique instance identifier.
      4)  Communications parameters (buffer sizes, max CLK speed, max burst size)
      5)  Current boot policy (e.g. A/B image selection, FW update state machine state)
      6)  Key ID(s) of trusted keys.
      7)  Key ID(s) of own keys.
  7)  Can support delivery of a properly signed firmware blob for installation.
  8)  Can transport alternate protocols if they become available.
 
Some of these items may be sufficiently supported by other means, such as
Sprockets and DICE. But when things go wrong with secured communications,
this sort of basic information is needed to support automated diagnosis
and repairs.

## Protocol

### Physical Layer SPI Protocol

The SP implements a basic SPI controller (master) and the RoT a SPI
target (slave). Master Out Slave In (MOSI) and Master In Slave Out
(MISO) are sampled on the rising edge of the clock signal (SCLK) when
Chip Select(CSn negated, active low) is asserted. The RoT indicates
that it has a response ready be asserting `ROT_IRQ` (also active low), and
deasserts at the end of the SPI frame (CSn deasserted).

#### Performance

In the case of the NXP LPC55, performance may be improved over the
initial programmed I/O implementation by using a wider FIFO configuration
(16-bits instead of 8), or by using a DMA controller.

The SP is currently required to use the slowest clock possible at about 800kHz.
Using DMA on the RoT should allow much faster clocking.

Note that if the programmed IO implementation is the one that first goes
to production, then the SP should use the slow clock and StatusReq to
query the RoT implementation and only configure a faster clock if the
RoT is able.

Alternatively, during the RoT's SWD bringup of the SP, SPI clock
configuration could be left for the SP.

#### Reliability

Programmed I/O is can be susceptible to underflow or overflow issues and
can be aggravated by changes in overall firmware behavior.  These problems
not become evident until firmware is more widely deployed.

Use of DMA can result in a better performing and therefore more reliable
implementation.

Though the SP to RoT connection is not seen as a risk for signal integrity
issues, one still needs to consider the possibility of transient errors
from hardware or software.

While physical attacks on the SP to RoT SPI connection are generally
considered out of scope, measures to improve data reliability can make
an attacker's job more challenging.

The RoT, as the target device, does not control the SPI clock. It does have
hardware that catches flow errors, i.e. receive overrun and transmit underrun.

The SP is able to set the pace and does not suffer flow errors and
also does not have direct evidence of RoT flow errors.

#### Data Integrity

The SPI physical layer does not have mechanisms to enhance data integrity.

Introducing a CRC to the message header makes detection on either side of the
interface much easier.

The addition of an explicit message, `ErrorRsp`, in the protocol, informs
the SP that the RoT did not successfully transmit or receive the previous message.

As long as the likelihood of a flow error is sufficiently low, overall performance of the SPI interface remains acceptable, even with retries. Only a small number of
retries (only one retry required for each failed transfer), has been needed in testing.

### Message Structure

A message consists of an eight byte header and a variable payload which
is not to exceed the length specified in the sprot-api crate.

    The first 8-bytes of a message received by the LPC55 are special
    in that they will fit completely into the receive FIFO and are
    therefore less susesptible to software induced overrun errors
    manifesting as dropped bytes.

    If the LPC55 FIFOs are configured for 16-bit widths, then 16 bytes
    message can be accomodated before lack of software service would
    result in a flow error.

    CRC32 is already in use to measure the LPC55 bootrom.
    In the interest of reducing code size, the same CRC32 is used
    in the header.

The message header is encoded in four bytes:

  - A Protocol identifier (`VERSION_1 = 0x01`),
  - enum MsgType as a `u8` describing the content of the message.
  - Payload length `U16<byteorder::LittleEndian>` length (LSB, MSB)
  - A 4-byte CRC32 over the header and payload without the CRC32 itself.

Initial supported protocols are:

 - 0x00, the null protocol. Any subsequent byte is ignored by the receiver.
 - 0x01, the protocol documented here (`VERSION_1`)
 - 0xff, reserved for internal use to denote an unsupported protocol
 - All others are reserved for future implementations.

The payload length is a u16 and may be zero bytes in cases where
the MsgType carries sufficient information to not carry a payload.
Error conditions are recognized where messages exceed the receiver's
buffering limits, there is a flow error, or where insufficient data is received.

The MsgType is used to allow for multiplexing the communications
link. While Sprockets is intended to be the primary user, MsgType allows
for satisfying the previously stated requirements for non-Sprockets
messages and for accommodating future work without invalidating the
needs for day-one interface stability.

The RoT exchanges data with the SP for as long as `CSn` is asserted and
the SP is clocking data. If the SP clocks more data in or out than the
RoT's buffers allow, the RoT will discard or zero-fill FIFO data until
the end of frame.

Note that it is common that the message being sent from the SP to the RoT
has a length different than the message being sent from the RoT to the SP.
The validity of any received message is based on message validation and, in the
case of the RoT, lack of any flow errors.

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

Message types include:

  - _Invalid_: 0x00 is reserved and invalid as a message type.
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
  - _Unknown_ - 0xff A reserved, internal representation for an unsupported message type.

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

    TODO: Testing needed for lengthy processing case such as firmware update.

If the pulse happens while Rx is idle the (possibly null) message that the RoT
has queued will be discarded and a null message will then be queued.

CSn pulse duration should be appropriately tuned to the implementation.

  1 Test with SP and RoT idle, fetch message from RoT and expect all zero bytes.
  2 Use lower-level SPI driver to send an `EchoReq`. Do not send pulse, Use lower level SPI driver to fetch RoT message, expect an `EchoRsp` or `ErrorRsp` if a correct CRC32 was not included in the header.
  3 Use lower-level SPI driver to send an `EchoReq`.
    See `ROT_IRQ` asserted.
    Send pulse.
    Use lower level SPI driver to fetch RoT message, expect all zeros.

#### RoT not ready to receive

The RoT has no explicit ready state visible to the SP other than the SP trying to send a message and getting a response.

The SP sending a CSn pulse, or using a timeout while sending a message seem to cover all the cases.

If an SP-visible busy condition was deemed useful, the RoT could fill its transmit FIFO with a `BUSY` code when busy and zeros or the next message to transmit when ready.

### Running tests

The implementation has been tested on Gemini and Gimletlet against the rot-carrier.

The RoT `EchoReq` and `EchoRsp` handling can be tested through the SP using
the existing humility spi command:

#### Test EchoReq and EchoRsp:

```sh
  $ SPI=3 # if Gimletlet
  $ SPI=4 # if Gemini
  $ APP=<path to app.toml>
  $ cargo xtask humility $APP -- spi -p $SPI -w 1,2,1,0,35,236,249,41,9
  ...
  [Ok([])]
  $ # Until the next command is executed, `ROT_IRQ` will be asserted.
  $ cargo xtask humility $APP -- spi -p $SPI -r -n9
               \/  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
  0x00000000 | 01 03 01 00 94 76 94 f5 09                      | .....v...
```

#### Test EchoReq and pulse CSn to clear RoT Tx and `ROT_IRQ`:

Same as above, but pulse CSn to clear the RoT Tx buffer.

```sh
$ cargo xtask humility $APP -- spi -p $SPI -w 1,2,1,0,35,236,249,41,9
...
[Ok([])]

$ cargo xtask humility $APP -- sprot --pulse 10
...
empty data: None
results=Ok([Ok([0x1, 0x0,],), ],)

# Note that the `humility sprot --pulse $DELAY` command returns the state of
`ROT_IRQ` before and after the pulse (asserted=1).

$ cargo xtask humility $APP -- spi -p $SPI -r -n6
..
humility: SPI master is spi3_driver
             \/  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
0x00000000 | 00 00 00 00 00 00                               | ......
$
```

#### Test sinking various count and size buffers from SP to RoT

##### Success for 10 messages with payload of 10 bytes (4 header + 10 payload)
```
slab:~/Oxide/src/hubris,gimletlet$ sp sprot --status
humility: attached to 0483:374f:0046001F3039510834393838 via ST-Link V3
subargs=SpRotArgs { send: None, pulse: None, sink: None, status: true, msgtype: "echoreq", timeout: 5000 }
ops=[Call(TargetFunction(34)), Done]
empty data: None
status=Status {
    supported: 0x00000002,
    bootrom_crc32: 0x47ae8b8d,
    rx_invalid: 0x00000001,
    rx_nop: 0x00000001,
    rx_overrun: 0x00000000,
    rx_received: 0x00000003,
    tx_incomplete: 0x00000000,
    tx_underrun: 0x00000000,
}
slab:~/Oxide/src/hubris,gimletlet$ rot reset
humility: Opened 1fc9:0143:MMLBHACB24IDJ via CMSIS-DAP
slab:~/Oxide/src/hubris,gimletlet$ sp reset
humility: Opened 0483:374f:0046001F3039510834393838 via ST-Link V3
slab:~/Oxide/src/hubris,gimletlet$ sp sprot -T10000 --sink 200,512
humility: attached to 0483:374f:0046001F3039510834393838 via ST-Link V3
subargs=SpRotArgs { send: None, pulse: None, sink: Some("200,512"), status: false, msgtype: "echoreq", timeout: 10000 }
ops=[Push32(200), Push32(512), Call(TargetFunction(33)), Done]
empty data: None
results=[
    Ok(
        [
            0xc8,
            0x0,
            0x0,
            0x0,
            0x0,
            0x0,
            0xf,
            0x0,
        ],
    ),
]
slab:~/Oxide/src/hubris,gimletlet$ sp sprot --status
humility: attached to 0483:374f:0046001F3039510834393838 via ST-Link V3
subargs=SpRotArgs { send: None, pulse: None, sink: None, status: true, msgtype: "echoreq", timeout: 5000 }
ops=[Call(TargetFunction(34)), Done]
empty data: None
status=Status {
    supported: 0x00000002,
    bootrom_crc32: 0x47ae8b8d,
    rx_invalid: 0x00000000,
    rx_nop: 0x00000001,
    rx_overrun: 0x00000012,
    rx_received: 0x000001af,
    tx_incomplete: 0x00000000,
    tx_underrun: 0x00000013,
}
```

The ErrorReq/ErrorRsp and SinkReq/SinkRsp messages are not expected to be useful in production and should be conditionally compiled or removed.

The returned SinkRsp message contains a SpRotSinkStatus struct:

```rust
#[repr(C, packed)]
pub struct SpRotSinkStatus {
    pub sent: u16,
    pub req_crc_err: u16,
    pub rsp_crc_err: u16,
    pub flow_err: u16,
}
```

So in the above test, 200 messages were sent by the RoT (`SinkRsp`) and there
were 15 flow errors. There were no CRC errors.

The test code in the SP successfully retried those 15 messages and returned `Ok`
for the test.

#### Test StatusReq and StatusRsp:

The exact values in the status structure are expected to change and stabilize
before first customer ship.

/Discussion on contents are appreciated/.

  $ cargo xtask humility $APP -- sprot --status
...
status=Status {
    supported: 0x00000002,
    bootrom_crc32: 0x47ae8b8d,
    rx_invalid: 0x00000000,
    rx_nop: 0x00000001,
    rx_overrun: 0x00000001,
    rx_received: 0x000000cd,
    tx_incomplete: 0x00000000,
    tx_underrun: 0x00000001,
}

```

#### Test sprockets messages from laptop RS232 to SP to RoT and back:

Connect a USB to RS232 adaptor, such as the "[OIKWAN USB Serial Adapter with FTDI Chipset](https://www.amazon.com/gp/product/B0759HSLP1)"

A "[9 Pin Serial Male to 10 Pin Motherboard Header Panel Mount Cable](https://www.amazon.com/StarTech-com-Serial-Motherboard-Header-Panel/dp/B0067DB6RU)" makes simple work of the connection. With these particular cables, a female/female DB9 adapter or Dupont wires between pins 2, 3, and 5 are needed to finish the connections.

Alternatively, use dupont wires directly to the Gemini J302 connector:

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

Identify the device path for your USB to RS232 adaptor. On the author's Linux laptop, it shows up as `/dev/ttyUSB0`.

```sh
SERIAL=/dev/ttyUSB0
```

In a [sprockets workspace](https://github.com/oxidecomputer/sprockets):

```sh
  cargo run -- -b 115200 -p $SERIAL get-certificates
  cargo run -- -b 115200 -p $SERIAL get-measurements
```

### Gimletlet Test Points

+------+------+------+------+
|      | MISO | RSTn | IRQn |
+------+------+------+------+
| GND  | SCK  | MOSI | CSn  |
+------+------+------+------+

## TODO/Issues

  - A general retry scheme may be useful to improve reliability. At the moment
    only the `sink` code in the SP demonstrates retries.
  - The SP may need to make a trade-off between SPI clock speed and reliability.
      - The error rate at different clock speeds should be characterized.
      - Stats on number of retries and other information related to reliability is needed to measure any improvements and to identify problems early.
  - Agree on identification information needed at earliest, unsecured boot for manufacturing and production workflows.
  - Agree on RoT firmware update workflows.
  - If more a more detailed MsgType::Error payload is useful, work that out early.
  - Complete testing is needed to ensure that the interface has a high degree of stability and reliability.
  - This doc belongs in an RFD.
