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

    let sock = UdpSocket::bind("[::]:0").await.unwrap();
    let mut consecutive_failures = 0;
    loop {
        let okay = run_one(&sock, addr).await;
        if okay {
            consecutive_failures = 0;
        } else {
            consecutive_failures += 1;
            if consecutive_failures > 10 {
                info!("too many consecutive failures; exiting\x07");
                break;
            }
        }
    }
}

async fn run_one(sock: &UdpSocket, addr: SocketAddrV6) -> bool {
    info!("sending packet to trigger high priority busy loop");
    sock.send_to(b"1", addr).await.unwrap();

    const PACKET_COUNT: usize = 10;
    for i in 1..PACKET_COUNT {
        info!("sending followup packet {i}");
        loop {
            match sock.send_to(format!("data-{i}").as_bytes(), addr).await {
                Ok(_) => break,
                Err(err) => {
                    error!("failed to send: {err}");
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
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
                if recvs >= 3 {
                    return true;
                } else if elapsed > Duration::from_secs(1) {
                    return false;
                }
            }
        }
    }
}
