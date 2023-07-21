from scapy.all import *

mac = "0e:1d:9a:64:b8:c2"
ip = "fe80::c1d:9aff:fe64:b8c2"

consecutive_failures = 0
pad = 58
iface = "enp0s25"

while True:
    base = Ether(dst=mac) / IPv6(dst=ip) / UDP(dport=7777, sport=2000)
    start = base / "1 hello, world"

    outer = Dot1Q(vlan=0x301, prio=0)
    poison = Ether(dst=mac) / outer / IPv6(dst=ip) / UDP(dport=7777, sport=2000)
    packets = []
    for i in range(1, 6):
        data = f"data-{i}" + '0' * pad
        packets.append(poison / data)

    sendp(start, iface=iface) # slow packet
    time.sleep(0.05)
    sendp(packets, iface=iface)
    time.sleep(0.25)
    sendp(packets, iface=iface)
