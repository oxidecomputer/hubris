from scapy.all import *

mac = "0e:1d:9a:64:b8:c2"
ip = "fe80::c1d:9aff:fe64:b8c2"

consecutive_failures = 0
pad = 16

while consecutive_failures < 2:
    print(datetime.now())
    print(pad)
    t = AsyncSniffer(filter = f"udp and host {ip} and (not ip6 multicast) and dst port 2000")
    t.start()
    base = Ether(dst=mac) / IPv6(dst=ip) / UDP(dport=7777, sport=2000)

    start = base / "1 hello, world"

    outer = Dot1Q(vlan=0x301, prio=0)
    poison = Ether(dst=mac) / outer / IPv6(dst=ip) / UDP(dport=7777, sport=2000)
    packets = []
    for i in range(1, 6):
        data = f"data-{i}" + '0' * pad
        packets.append(poison / data)

    sendp(start) # slow packet
    time.sleep(0.05)
    sendp(packets)
    time.sleep(0.25)
    t.stop()

    for p in t.results:
        print(p[3].load.decode('ascii'))
    if len(t.results) == 0:
        print("got a failure!\a")
        consecutive_failures += 1
    else:
        pad += 1
        if pad == 128:
            pad = 16
        consecutive_failures = 0
print(pad)
