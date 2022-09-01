use std::env;
use std::net::SocketAddrV6;
use std::time::Duration;
use std::time::Instant;

use log::info;
use tokio::net::UdpSocket;

#[tokio::main]
async fn main() {
    env_logger::init();

    let args = env::args().collect::<Vec<_>>();
    let addr = args.get(1).expect("pass addr as command line arg");
    let addr: SocketAddrV6 = addr.parse().unwrap();

    let sock = UdpSocket::bind("[::]:0").await.unwrap();
    loop {
        run_one(&sock, addr).await;
    }
}

async fn run_one(sock: &UdpSocket, addr: SocketAddrV6) {
    info!("sending packet to trigger high priority busy loop");
    sock.send_to(b"1", addr).await.unwrap();

    for i in 1..10 {
        info!("sending followup packet {i}");
        sock.send_to(format!("data-{i}").as_bytes(), addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let mut recvs = 0;
    let mut buf = [0; 64];
    let start = Instant::now();
    loop {
        match tokio::time::timeout(Duration::from_secs(1), sock.recv_from(&mut buf)).await {
            Ok(result) => {
                let (_n, peer) = result.unwrap();
                recvs += 1;
                info!("received response {recvs} from {peer}");
            }
            Err(_) => {
                info!("no response after {:?}", start.elapsed());
                if recvs >= 3 {
                    return;
                }
            }
        }
    }
}
