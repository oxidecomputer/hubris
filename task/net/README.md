# About
The `net` task implements a small netstack based on [_smoltcp_](https://github.com/smoltcp-rs/smoltcp)

# Design of the SP Network Stack

NOTE: This network stack is specialized to the SP's needs and is not, at the
moment, intended as a fully general network stack.

In particular, we currently only support IP and UDP, with a focus on IPv6.

## The Net Task

The `net` task is responsible for maintaining system network state. It includes:

1. The Ethernet MAC and PHY drivers.
2. Ethernet DMA support and buffer management.
3. The IP stack itself (_smoltcp_).
4. Buffers for each socket defined in the system.

The `net` task itself is designed to manage network interface events as quickly
as possible, without blocking on other tasks. It sends no IPCs during its normal
operation after configuring any switches or PHYs.

It exposes an IPC interface that other tasks can use to request services, like
sending or receiving packets.

## Global configuration

One useful property of our application is that we can predict the full set of
required network resources --- buffers, sockets, and the like --- at compile
time, and allocate them statically.

This information is defined in the `app.toml` file's `config.net` section. Here
is an example of a UDP echo service (port 7) in the current version of the
netstack:

```toml
[config.net.sockets.echo]
kind = "udp"
owner = {name = "udpecho", notification = "socket"}
port = 7
tx = { packets = 3, bytes = 1024 }
rx = { packets = 3, bytes = 1024 }
```

and the corresponding task gets configured with the following relevant bits
(normal task stuff omitted):

```toml
[tasks.udpecho]
# other stuff omitted
task-slots = ["net"]
notifications = ["socket"]
```

Sockets have a system-wide unique name (here, `echo`) and an "owner" task (here,
`udpecho`). Only the owner can interact with the socket. When events occur on
the socket, the network stack will post the given `notification` to the owning
task.

The `tx` and `rx` sections define the number of buffers to allocate for metadata
of received `packets`, and the total number of `bytes` to allocate to store
those packets' payloads.

## IPC interface

From the perspective of a client task, such as `udpecho` above, the network
stack implements two IPC operations, both of which are _prompt_ -- the netstack
will process the request when it's received, and then return, rather than
blocking the caller until some event occurs.

`send_packet` takes a socket identifier, information about the destination of
the packet (address and port, for UDP) and the payload of the packet as a lease.
It asks the network stack to take one of the socket's tx buffers and copy the
packet into it. On success, the caller can repurpose the buffer that held the
packet payload, because the network stack now has a copy. If all the socket's tx
buffers are full, it will return an error.

`recv_packet` takes a socket identifier and a byte buffer as a lease. It asks
the network stack to dequeue the next packet waiting on that socket and copy it
into the leased buffer, returning metadata (source address and port). If no
packet is waiting, it will return an error.

Finally, whenever new activity occurs on a socket, the network stack will post
the configured notification to the socket's owner. This means a server can
service other requests and do useful work without sitting blocked on the
netstack forever. When it notices the netstack's notification, it can turn
around and talk to the netstack, which will return promptly without blocking.

## Life of a UDP exchange / every byte copy

This section describes every time a byte is transferred during a UDP
receive-send pair. In total, not counting internal FIFOs within the Ethernet
controller, each byte in a received UDP packet being routed to a valid socket
associated with a task is written to memory three times. First, by the
controller into a central DMA queue. Second, by the netstack into a
socket-specific queue. Third, by the task that owns the socket, to transfer the
data into its address space. The copy count for transmitted data is similar.

We could potentially eliminate one of these copies by using a much fancier
buffer pool management algorithm in the netstack, at the cost of complexity.

Here is the current actual flow for a UDP request/response.

1. Packet arrives from the Ethernet PHY into the MAC's receive FIFO.

2. MAC notices packet is complete and verifies checksums, etc. It begins
transferring the packet out of the FIFO and into a reserved area of memory using
DMA.

3. Once the DMA transfer is complete, the MAC generates an interrupt.

4. The Hubris kernel handles the interrupt and notifies the `net` task.

5. Once anything higher-priority has yielded the CPU, the `net` task gets the
notification and inspects the packet in-place in DMA memory using _smoltcp_.
Some packets, such as neighbor discovery and ICMP echo, are handled at this
stage without a further copy. UDP packets are matched to a socket; if no
matching socket exists, they are discarded with a Destination Port Unreachable
response.

6. If the matching socket has free buffer space, the packet is copied into the
socket's buffer, and the DMA buffer is returned to the hardware to receive more
packets. The `net` task notifies the task that owns the socket. If a packet is
received for a socket that is out of space, the `net` task drops it, increments
a counter on the socket, and returns the buffer to the DMA pool. (This copy is
performed to ensure that a task that fails to read its socket in a timely
fashion only stalls packets to _that socket,_ rather than starving the Ethernet
DMA engine of buffers.)

7. Assuming the owning task had blocked waiting for a socket event, it will wake
up due to the notification once the `net` task and anything else higher priority
have yielded. It asks for details of the event that woke it by sending a
`recv_packet` message to `net`, loaning a writable buffer. The `net` task
consults the socket, and, if the packet fits, copies it into the loaned memory,
freeing the corresponding socket buffer to receive more packets.

8. The owning task inspects the packet and does whatever it needs to. Let's
assume that it generates a reply into a buffer.

9. The owning task sends a `send_packet` message to `net`, loaning (read-only)
an out-going packet.

10. If the socket has buffer space, `net` accepts the packet and copies it into
the socket buffer (in `net`'s memory). If the socket does _not_ have outgoing
space, the `net` task returns an error to the caller. (This copy is important
to ensure that `net` has timely access to the packet contents when it becomes
time to transmit.)

11. `net` polls _smoltcp_ to trigger transmission. If there is an Ethernet DMA
buffer available, we copy it into the DMA buffer and send it to the hardware.
(If not, we wait until a buffer is freed by the transmission of a different
packet.) (This copy is important to ensure that a task sending packets faster
than the Ethernet interface can run only consumes _its_ socket buffers, not a
disproportionate share of the Ethernet DMA buffer pool.)

12. When the Ethernet MAC DMA engine reaches the packet, it reads it out of RAM
into the TX FIFO, inserting checksum calculations along the way.

13. The Ethernet MAC pops the packet from the TX FIFO and puts it on the RMII
interface to the PHY, where it hits the wire without further buffering.


# VLAN support
## Configuration and build
VLAN support is enabled through the `vlan` feature in the `net` task, and
`vlan` features in all dependent crates. This is checked in the `net-api` build
script, which raises an error if it's not enabled for all relevant crates
(e.g. if you enabled it for the `net` task but forgot to enable it for the
`udpecho` task, which uses `net-api`).

When the feature is enabled, the build system reads a `vlan` dictionary from
`[config.net]`, which specifies the start VLAN (as a VID) and the total number
of VLANs. This information is used to generate **arrays of arrays** in the
`net` build system; where we would previously make a single array of
`SOCKET_COUNT` items, we're now building a nested array with
`SOCKET_COUNT * VLanId::LENGTH` total items.

## Basic architecture
Each VLAN runs an independent instance of _smoltcp_ with `SOCKET_COUNT`
independent sockets. These instances are VLAN-unaware; they think that
everything is normal. Instead of owning the `Ethernet` device directly, they
each own a `VLanEthernet` facade. This facade stores a (non-mutable) reference
to the `Ethernet` peripheral and a VID.

### Receive path
VLAN tags are stripped by the STM32H7's ethernet peripheral. The tag's presence
and value (VID) are available in the RDES DMA descriptor. When a _smoltcp_
instance is polled, it calls into its `VLanEthernet` to check if packets are
available. This checks for both an available descriptor _and_ that the stripped
VID matches the `VLanEthernet`'s VID.

Naively, this lead to congestion: if a packet arrives that doesn't match any
VLAN, each instance would refuse to receive it. To avoid this issue, the
check _also_ **discards invalid packets**. Invalid packets include
- Packets that did not have a VID
- Packets with a VID outside of the entire VLAN VID range
- Descriptors with the error flags set
- Descriptors without first and last bits set (`FD` / `LD`)

The `can_recv` function continues to discard invalid packets until either the
ring buffer is empty, or the next packet is valid. Note that it doesn't have
to be valid for _the particular _smoltcp_ instance that called `can_recv`_;
as long as it has VID that matches _one_ of the instances, it will be consumed
eventually.

The VLAN receive path should be infallible once `can_recv` returns true. Clever
lifetime tricks using the `RxToken` ensure that no one else can take packets
while it's held by the interface, meaning the validity of `can_recv` should
not change.

(This isn't 100% foolproof, because someone else could call into the `Ethernet`
peripheral directly, but it's relatively easy to audit: `can_recv` is only
called by `VLanEthernet::receive`, generating a token, and `vlan_recv` is only
called by `VLanRxToken::consume`)

### Transmit path
When transmitting in VLAN mode, we use DMA _context descriptors_ to set the
VLAN tag, which is then automatically injected into packets. For simplicity,
we _always_ set the VLAN tag with a context descriptor before writing the
transmit descriptor. This doubles the size of our Tx descriptor ring, since
each item is now `[[u32; 4]; 2]` instead of `[u32; 4]`.

Other than that, the Tx path is largely the same as the non-VLAN code.

### Task API
To tasks seeking to communicate on the network, the API is _nearly_ the same.
They still use `send_packet` and `recv_packet` IPC calls to the `net` API, and
use the same socket numbering as before.

However, the `struct UdpMetadata` used in these calls has an extra field when
the `vlan` feature is enabled: `vid: u16`.  This `vid` directs the packet
in `send_packet` to a particular VLAN, and is returned in `recv_packet` so
that the caller can know the source VLAN of a packet.

(This is why all tasks must agree to turn on the `vlan` feature; otherwise,
some tasks will build `net-api` without this field, which will break inter-task
communication)

If the task is only replying to packets (i.e. `udpecho`), this is invisible:
the task simply includes the source `meta` in its reply, so the reply goes
out to the same VLAN.

If a task is _generating_ packets independently, it has to think a little more
about where the packets are going, which is good and intentional given our
system design.
