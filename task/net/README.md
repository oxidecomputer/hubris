# About
The `net` task implements a small netstack based on [_smoltcp_](https://github.com/smoltcp-rs/smoltcp)

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
`SOCKET_COUNT * VLAN_COUNT` total items.

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
