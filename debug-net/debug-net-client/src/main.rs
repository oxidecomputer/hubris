use std::env;
use std::net::SocketAddrV6;
use std::time::Duration;
use std::time::Instant;

use log::error;
use log::info;
use tokio::net::UdpSocket;

#[tokio::main]
async fn main() {
    env_logger::init();

    let args = env::args().collect::<Vec<_>>();
    let addr = args.get(1).expect("pass addr as command line arg");
    let addr: SocketAddrV6 = addr.parse().unwrap();
    let start_time = Instant::now();

    let pad_size: Option<usize> = args.get(2).map(|s| s.parse().unwrap());

    let sock = UdpSocket::bind("[::]:0").await.unwrap();
    let mut consecutive_failures = 0;
    const PAD_MIN: usize = 32;
    const PAD_MAX: usize = 64;
    let mut pad = PAD_MIN;
    loop {
        let okay = run_one(&sock, addr, pad_size.unwrap_or(pad)).await;
        if okay {
            consecutive_failures = 0;
            pad += 1;
            if pad > PAD_MAX {
                pad = PAD_MIN;
            }
        } else {
            consecutive_failures += 1;
            if consecutive_failures > 4 {
                info!("too many consecutive failures; exiting\x07");
                break;
            }
        }
    }

    println!(
        "finished in {:?} with pad {}",
        Instant::now() - start_time,
        pad_size.unwrap_or(pad)
    );
}

async fn run_one(sock: &UdpSocket, addr: SocketAddrV6, pad: usize) -> bool {
    info!("sending packet to trigger high priority busy loop, pad {pad}");
    sock.send_to(b"1", addr).await.unwrap();
    // Wait for the SP to enter its busy-sleep
    tokio::time::sleep(Duration::from_millis(50)).await;

    const PACKET_COUNT: usize = 6;

    let mut packets = vec![];
    for i in 1..PACKET_COUNT {
        let mut d = format!("data-{i}").as_bytes().to_owned();
        for i in 0..pad {
            d.push(b'0' + (i % 10) as u8);
        }
        packets.push(d);
    }

    for (i, d) in packets.iter().enumerate() {
        info!("sending followup packet {i}");
        loop {
            match sock.send_to(&d, addr).await {
                Ok(_) => break,
                Err(err) => {
                    error!("failed to send: {err}");
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }
        }
    }

    let mut recvs = 0;
    let mut buf = [0; 64];
    let start = Instant::now();
    loop {
        match tokio::time::timeout(
            Duration::from_millis(100),
            sock.recv_from(&mut buf),
        )
        .await
        {
            Ok(result) => {
                let (n, peer) = result.unwrap();
                recvs += 1;
                let s = std::str::from_utf8(&buf[..n]).unwrap();
                info!("received response {recvs} '{s}' from {peer}");
                if recvs == PACKET_COUNT {
                    return true;
                }
            }
            Err(_) => {
                let elapsed = start.elapsed();
                info!("no response after {elapsed:?}");
                if recvs > 0 {
                    return true;
                } else if elapsed > Duration::from_secs(2) {
                    return false;
                }
            }
        }
    }
}
